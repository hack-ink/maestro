use std::{
	collections::{HashMap, HashSet},
	fs,
	path::{Path, PathBuf},
	process::Command,
	slice, thread,
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use color_eyre::Report;
use directories::ProjectDirs;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
	agent::{
		self, AppServerRunRequest, ISSUE_COMMENT_TOOL_NAME, ISSUE_LABEL_ADD_TOOL_NAME,
		ISSUE_TRANSITION_TOOL_NAME,
	},
	config::{self, ServiceConfig},
	prelude::{Result, eyre},
	state::{StateStore, WorktreeMapping},
	tracker::{IssueTracker, TrackerIssue, linear::LinearClient},
	workflow::WorkflowDocument,
	workspace::{WorkspaceManager, WorkspaceSpec},
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct RunSummary {
	project_id: String,
	issue_identifier: String,
	branch_name: String,
	worktree_path: PathBuf,
	attempt_number: i64,
	run_id: String,
}

#[derive(Clone, Debug)]
struct IssueRunPlan {
	issue: TrackerIssue,
	workspace: WorkspaceSpec,
	attempt_number: i64,
	run_id: String,
}

pub(crate) fn run_once(config_path: Option<&Path>, dry_run: bool) -> Result<()> {
	let Some(config_path) = resolve_config_path(config_path)? else {
		if dry_run {
			println!("dry run: no maestro config found; nothing to execute.");

			return Ok(());
		}

		eyre::bail!("No maestro config found. Pass --config or create maestro.toml.");
	};
	let state_store = if dry_run {
		StateStore::open_in_memory()?
	} else {
		StateStore::open(default_state_store_path()?)?
	};

	if let Some(summary) = run_configured_cycle(&config_path, &state_store, dry_run)? {
		if dry_run {
			println!(
				"dry run: project={} issue={} branch={} worktree={} attempt={}",
				summary.project_id,
				summary.issue_identifier,
				summary.branch_name,
				summary.worktree_path.display(),
				summary.attempt_number
			);
		} else {
			println!(
				"run complete: project={} issue={} run_id={} worktree={}",
				summary.project_id,
				summary.issue_identifier,
				summary.run_id,
				summary.worktree_path.display()
			);
		}

		return Ok(());
	}

	println!("No eligible issue found for the configured project.");

	Ok(())
}

pub(crate) fn run_daemon(config_path: Option<&Path>, poll_interval: Duration) -> Result<()> {
	if poll_interval.is_zero() {
		eyre::bail!("`daemon --poll-interval-s` must be greater than zero.");
	}

	let Some(config_path) = resolve_config_path(config_path)? else {
		eyre::bail!("No maestro config found. Pass --config or create maestro.toml.");
	};
	let state_store = StateStore::open(default_state_store_path()?)?;

	tracing::info!(
		config_path = %config_path.display(),
		poll_interval_s = poll_interval.as_secs(),
		"Starting daemon poll loop."
	);

	loop {
		let tick_started_at = Instant::now();

		match run_configured_cycle(&config_path, &state_store, false) {
			Ok(Some(summary)) => {
				println!(
					"run complete: project={} issue={} run_id={} worktree={}",
					summary.project_id,
					summary.issue_identifier,
					summary.run_id,
					summary.worktree_path.display()
				);
			},
			Ok(None) => {
				tracing::debug!("Daemon tick found no eligible issue.");
			},
			Err(error) => {
				tracing::warn!(?error, "Daemon tick failed.");
			},
		}

		sleep_until_next_tick(poll_interval, tick_started_at);
	}
}

fn run_configured_cycle(
	config_path: &Path,
	state_store: &StateStore,
	dry_run: bool,
) -> Result<Option<RunSummary>> {
	let config = ServiceConfig::from_path(config_path)?;
	let workflow_path = config.repo_root().join(config.workflow_path());
	let workflow = WorkflowDocument::from_path(&workflow_path)?;
	let api_key = config.tracker().resolve_api_key()?;
	let tracker = LinearClient::new(api_key)?;

	run_project_once(&tracker, &config, &workflow, state_store, dry_run)
}

fn run_project_once<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	dry_run: bool,
) -> Result<Option<RunSummary>>
where
	T: IssueTracker,
{
	let workspace_manager =
		WorkspaceManager::new(project.id(), project.repo_root(), project.workspace_root());

	if !dry_run {
		reconcile_project_state(tracker, project, workflow, state_store, &workspace_manager)?;
	}

	validate_project_contract(project, workflow)?;
	validate_tracker_project(tracker, project.tracker().project_slug())?;

	let project_slug = project.tracker().project_slug();
	let issues = tracker.list_project_issues(project_slug)?;
	let Some(issue) = issues
		.into_iter()
		.find(|issue| is_issue_eligible(issue, workflow, state_store).unwrap_or(false))
	else {
		return Ok(None);
	};
	let mut refreshed_issues = tracker.refresh_issues(slice::from_ref(&issue.id))?;
	let Some(issue) = refreshed_issues.pop() else {
		return Ok(None);
	};

	if !is_issue_eligible(&issue, workflow, state_store)? {
		return Ok(None);
	}

	let attempt_number = state_store.next_attempt_number(&issue.id)?;
	let run_id = build_run_id(&issue.identifier, attempt_number)?;
	let workspace = workspace_manager.ensure_workspace(&issue.identifier, dry_run)?;

	if !dry_run {
		state_store.upsert_worktree(
			project.id(),
			&issue.id,
			&workspace.branch_name,
			&workspace.path.display().to_string(),
		)?;
	}

	let Some(issue) = refresh_issue(tracker, &issue.id)? else {
		return Ok(None);
	};

	if !is_issue_eligible(&issue, workflow, state_store)? {
		if !dry_run && is_terminal_issue(&issue, workflow) {
			cleanup_terminal_worktree(state_store, &workspace_manager, &issue.id, &workspace.path)?;
		}

		return Ok(None);
	}

	let issue_run = IssueRunPlan { issue, workspace, attempt_number, run_id };

	if dry_run {
		return Ok(Some(RunSummary {
			project_id: project.id().to_owned(),
			issue_identifier: issue_run.issue.identifier.clone(),
			branch_name: issue_run.workspace.branch_name.clone(),
			worktree_path: issue_run.workspace.path.clone(),
			attempt_number: issue_run.attempt_number,
			run_id: issue_run.run_id.clone(),
		}));
	}

	let summary = execute_issue_run(tracker, project, workflow, state_store, issue_run)?;

	Ok(Some(summary))
}

fn reconcile_project_state<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	workspace_manager: &WorkspaceManager,
) -> Result<()>
where
	T: IssueTracker,
{
	let leases = state_store.list_leases(project.id())?;
	let worktrees = state_store.list_worktrees(project.id())?;

	if leases.is_empty() && worktrees.is_empty() {
		return Ok(());
	}

	let mut issue_ids = HashSet::new();

	for lease in &leases {
		issue_ids.insert(lease.issue_id().to_owned());
	}
	for mapping in &worktrees {
		issue_ids.insert(mapping.issue_id().to_owned());
	}

	let refreshed_issues = tracker.refresh_issues(&issue_ids.into_iter().collect::<Vec<_>>())?;
	let issues_by_id = refreshed_issues
		.into_iter()
		.map(|issue| (issue.id.clone(), issue))
		.collect::<HashMap<_, _>>();

	for lease in &leases {
		let reconciled_status = match issues_by_id.get(lease.issue_id()) {
			Some(issue) if is_terminal_issue(issue, workflow) => "terminated",
			Some(_) | None => "interrupted",
		};

		mark_run_attempt_if_active(state_store, lease.run_id(), reconciled_status)?;

		state_store.clear_lease(lease.issue_id())?;
	}
	for mapping in &worktrees {
		if issues_by_id
			.get(mapping.issue_id())
			.is_some_and(|issue| is_terminal_issue(issue, workflow))
		{
			cleanup_worktree_mapping(state_store, workspace_manager, mapping)?;
		}
	}

	Ok(())
}

fn validate_tracker_project<T>(tracker: &T, project_slug: &str) -> Result<()>
where
	T: IssueTracker,
{
	tracker
		.get_project_by_slug(project_slug)?
		.ok_or_else(|| eyre::eyre!("Linear project slug `{project_slug}` was not found."))?;

	Ok(())
}

fn execute_issue_run<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	issue_run: IssueRunPlan,
) -> Result<RunSummary>
where
	T: IssueTracker,
{
	state_store.upsert_lease(project.id(), &issue_run.issue.id, &issue_run.run_id)?;
	state_store.upsert_worktree(
		project.id(),
		&issue_run.issue.id,
		&issue_run.workspace.branch_name,
		&issue_run.workspace.path.display().to_string(),
	)?;

	let result = execute_issue_run_inner(tracker, project, workflow, state_store, &issue_run);

	state_store.clear_lease(&issue_run.issue.id)?;

	match result {
		Ok(summary) => Ok(summary),
		Err(error) => {
			handle_failure(tracker, project, workflow, &issue_run, &error)?;

			Err(error)
		},
	}
}

fn execute_issue_run_inner<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	issue_run: &IssueRunPlan,
) -> Result<RunSummary>
where
	T: IssueTracker,
{
	let transport = project
		.agent()
		.transport()
		.unwrap_or(workflow.frontmatter().agent().transport())
		.to_owned();
	let model =
		project.agent().model().or(workflow.frontmatter().agent().model()).map(str::to_owned);
	let tracker_tool_bridge =
		crate::agent::TrackerToolBridge::new(tracker, &issue_run.issue, workflow);

	agent::execute_app_server_run(
		&AppServerRunRequest {
			run_id: issue_run.run_id.clone(),
			issue_id: issue_run.issue.id.clone(),
			attempt_number: issue_run.attempt_number,
			listen: transport,
			cwd: issue_run.workspace.path.display().to_string(),
			approval_policy: workflow.frontmatter().agent().approval_policy().to_owned(),
			sandbox: workflow.frontmatter().agent().sandbox().to_owned(),
			developer_instructions: build_developer_instructions(project, workflow, issue_run)?,
			user_input: build_user_input(&issue_run.issue, workflow, issue_run),
			model: model.clone(),
			personality: workflow.frontmatter().agent().personality().map(str::to_owned),
			service_tier: workflow.frontmatter().agent().service_tier().map(str::to_owned),
			timeout: Duration::from_secs(300),
			dynamic_tool_handler: Some(&tracker_tool_bridge),
		},
		state_store,
	)?;

	run_validation_commands(
		workflow.frontmatter().execution().validation_commands(),
		&issue_run.workspace.path,
	)?;

	Ok(RunSummary {
		project_id: project.id().to_owned(),
		issue_identifier: issue_run.issue.identifier.clone(),
		branch_name: issue_run.workspace.branch_name.clone(),
		worktree_path: issue_run.workspace.path.clone(),
		attempt_number: issue_run.attempt_number,
		run_id: issue_run.run_id.clone(),
	})
}

fn handle_failure<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	issue_run: &IssueRunPlan,
	error: &Report,
) -> Result<()>
where
	T: IssueTracker,
{
	let max_attempts = i64::from(workflow.frontmatter().execution().max_attempts());

	if issue_run.attempt_number < max_attempts {
		tracker.create_comment(
			&issue_run.issue.id,
			&format_retry_comment(
				&issue_run.run_id,
				issue_run.attempt_number,
				max_attempts,
				relative_worktree_path(project, &issue_run.workspace),
				&issue_run.workspace.branch_name,
				error,
			),
		)?;

		return Ok(());
	}

	let failure_state_id = issue_run
		.issue
		.state_id_for_name(workflow.frontmatter().tracker().failure_state())
		.ok_or_else(|| {
			eyre::eyre!(
				"State `{}` was not found for issue `{}`.",
				workflow.frontmatter().tracker().failure_state(),
				issue_run.issue.identifier
			)
		})?;

	tracker.update_issue_state(&issue_run.issue.id, failure_state_id)?;

	if let Some(label_id) =
		issue_run.issue.label_id_for_name(workflow.frontmatter().tracker().needs_attention_label())
	{
		let mut label_ids =
			issue_run.issue.labels.iter().map(|label| label.id.clone()).collect::<Vec<_>>();

		if !label_ids.iter().any(|existing| existing == label_id) {
			label_ids.push(label_id.to_owned());
			tracker.update_issue_labels(&issue_run.issue.id, &label_ids)?;
		}
	} else {
		tracing::warn!(
			label = workflow.frontmatter().tracker().needs_attention_label(),
			issue = issue_run.issue.identifier,
			"Needs-attention label was not found in the issue team."
		);
	}

	tracker.create_comment(
		&issue_run.issue.id,
		&format_terminal_failure_comment(
			&issue_run.run_id,
			issue_run.attempt_number,
			relative_worktree_path(project, &issue_run.workspace),
			&issue_run.workspace.branch_name,
			error,
		),
	)?;

	Ok(())
}

fn validate_project_contract(project: &ServiceConfig, workflow: &WorkflowDocument) -> Result<()> {
	if project.tracker().project_slug() != workflow.frontmatter().tracker().project_slug() {
		eyre::bail!(
			"Project config tracker slug `{}` does not match WORKFLOW.md tracker slug `{}`.",
			project.tracker().project_slug(),
			workflow.frontmatter().tracker().project_slug()
		);
	}

	Ok(())
}

fn is_issue_eligible(
	issue: &TrackerIssue,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
) -> Result<bool> {
	let tracker_policy = workflow.frontmatter().tracker();

	if tracker_policy.terminal_states().iter().any(|state| state == &issue.state.name) {
		return Ok(false);
	}
	if !tracker_policy.startable_states().iter().any(|state| state == &issue.state.name) {
		return Ok(false);
	}
	if issue.has_label(tracker_policy.opt_out_label()) {
		return Ok(false);
	}

	Ok(state_store.lease_for_issue(&issue.id)?.is_none())
}

fn refresh_issue<T>(tracker: &T, issue_id: &str) -> Result<Option<TrackerIssue>>
where
	T: IssueTracker,
{
	let issue_ids = [issue_id.to_owned()];
	let mut refreshed_issues = tracker.refresh_issues(&issue_ids)?;

	Ok(refreshed_issues.pop())
}

fn is_terminal_issue(issue: &TrackerIssue, workflow: &WorkflowDocument) -> bool {
	workflow
		.frontmatter()
		.tracker()
		.terminal_states()
		.iter()
		.any(|state| state == &issue.state.name)
}

fn mark_run_attempt_if_active(
	state_store: &StateStore,
	run_id: &str,
	reconciled_status: &str,
) -> Result<()> {
	let Some(run_attempt) = state_store.run_attempt(run_id)? else {
		return Ok(());
	};

	if matches!(run_attempt.status(), "starting" | "running") {
		state_store.update_run_status(run_id, reconciled_status)?;
	}

	Ok(())
}

fn cleanup_worktree_mapping(
	state_store: &StateStore,
	workspace_manager: &WorkspaceManager,
	mapping: &WorktreeMapping,
) -> Result<()> {
	workspace_manager.remove_workspace_path(mapping.worktree_path())?;
	state_store.clear_worktree(mapping.issue_id())?;

	Ok(())
}

fn cleanup_terminal_worktree(
	state_store: &StateStore,
	workspace_manager: &WorkspaceManager,
	issue_id: &str,
	worktree_path: &Path,
) -> Result<()> {
	workspace_manager.remove_workspace_path(worktree_path)?;
	state_store.clear_worktree(issue_id)?;

	Ok(())
}

fn build_developer_instructions(
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	issue_run: &IssueRunPlan,
) -> Result<String> {
	let mut sections = Vec::new();

	for relative_path in workflow.frontmatter().context().read_first() {
		let absolute_path = project.repo_root().join(relative_path);
		let contents = fs::read_to_string(&absolute_path)?;

		sections.push(format!("File: {relative_path}\n{contents}"));
	}

	if !workflow.body().is_empty() {
		sections.push(format!("WORKFLOW.md\n{}", workflow.body()));
	}

	sections.push(format!(
		"Tracker tool contract\n- You own issue-scoped tracker writes for `{issue}`.\n- At the start of execution, call `{transition_tool}` to move the issue to `{in_progress}` and add a brief `{comment_tool}` comment that you started work on run `{run_id}` attempt `{attempt}`.\n- When implementation and repo validation are complete, call `{transition_tool}` to move the issue to `{success}` and add a brief `{comment_tool}` comment summarizing the result.\n- If you determine the issue needs human attention, add label `{needs_attention}` with `{label_tool}` and explain why in a comment.\n- Never write to any other issue.",
		issue = issue_run.issue.identifier,
		transition_tool = ISSUE_TRANSITION_TOOL_NAME,
		comment_tool = ISSUE_COMMENT_TOOL_NAME,
		label_tool = ISSUE_LABEL_ADD_TOOL_NAME,
		in_progress = workflow.frontmatter().tracker().in_progress_state(),
		run_id = issue_run.run_id,
		attempt = issue_run.attempt_number,
		success = workflow.frontmatter().tracker().success_state(),
		needs_attention = workflow.frontmatter().tracker().needs_attention_label(),
	));

	Ok(sections.join("\n\n"))
}

fn build_user_input(
	issue: &TrackerIssue,
	workflow: &WorkflowDocument,
	issue_run: &IssueRunPlan,
) -> String {
	format!(
		"Resolve Linear issue {identifier}: {title}\n\nDescription:\n{description}\n\nExecution checklist:\n- Move the issue to `{in_progress}` with `{transition_tool}` and leave a short `{comment_tool}` comment that includes run `{run_id}` attempt `{attempt}`.\n- Implement the fix in the current worktree.\n- Run the repository validation needed to justify moving the issue to `{success}`.\n- When done, move the issue to `{success}` with `{transition_tool}` and leave a short `{comment_tool}` completion comment with the result.\n- If the issue needs manual attention, add label `{needs_attention}` with `{label_tool}` and explain why in a comment.",
		identifier = issue.identifier,
		title = issue.title,
		description = if issue.description.trim().is_empty() {
			String::from("(no description)")
		} else {
			issue.description.clone()
		},
		transition_tool = ISSUE_TRANSITION_TOOL_NAME,
		comment_tool = ISSUE_COMMENT_TOOL_NAME,
		label_tool = ISSUE_LABEL_ADD_TOOL_NAME,
		in_progress = workflow.frontmatter().tracker().in_progress_state(),
		run_id = issue_run.run_id,
		attempt = issue_run.attempt_number,
		success = workflow.frontmatter().tracker().success_state(),
		needs_attention = workflow.frontmatter().tracker().needs_attention_label(),
	)
}

fn run_validation_commands(commands: &[String], cwd: &Path) -> Result<()> {
	for command in commands {
		let output = Command::new("zsh").arg("-lc").arg(command).current_dir(cwd).output()?;

		if !output.status.success() {
			let stderr = String::from_utf8_lossy(&output.stderr);

			eyre::bail!(
				"Validation command `{}` failed in `{}`: {}",
				command,
				cwd.display(),
				stderr.trim()
			);
		}
	}

	Ok(())
}

fn relative_worktree_path(project: &ServiceConfig, workspace: &WorkspaceSpec) -> String {
	if let Ok(relative_path) = workspace.path.strip_prefix(project.repo_root()) {
		return relative_path.display().to_string();
	}
	if let Some(root_name) = project.workspace_root().file_name()
		&& let Ok(relative_path) = workspace.path.strip_prefix(project.workspace_root())
	{
		return Path::new(root_name).join(relative_path).display().to_string();
	}

	workspace.path.file_name().map_or_else(
		|| workspace.path.display().to_string(),
		|path| path.to_string_lossy().into_owned(),
	)
}

fn format_retry_comment(
	run_id: &str,
	attempt_number: i64,
	max_attempts: i64,
	worktree_path: String,
	branch_name: &str,
	error: &Report,
) -> String {
	format!(
		"maestro run failed and will retry\n\n- run_id: `{run_id}`\n- attempt: `{attempt_number}` / `{max_attempts}`\n- failed_at: `{failed_at}`\n- branch: `{branch}`\n- worktree_path: `{worktree}`\n- error_class: `retryable_execution_failure`\n- next_action: `maestro will retry automatically`\n- error: `{error}`",
		failed_at = current_timestamp(),
		branch = branch_name,
		worktree = worktree_path
	)
}

fn format_terminal_failure_comment(
	run_id: &str,
	attempt_number: i64,
	worktree_path: String,
	branch_name: &str,
	error: &Report,
) -> String {
	format!(
		"maestro run failed and needs attention\n\n- run_id: `{run_id}`\n- attempt: `{attempt_number}`\n- failed_at: `{failed_at}`\n- branch: `{branch}`\n- worktree_path: `{worktree}`\n- error_class: `retry_budget_exhausted`\n- next_action: `inspect the worktree, resolve the issue manually, then move the issue back to a startable state if another automated run is desired`\n- error: `{error}`",
		failed_at = current_timestamp(),
		branch = branch_name,
		worktree = worktree_path
	)
}

fn current_timestamp() -> String {
	OffsetDateTime::now_utc().format(&Rfc3339).expect("timestamp formatting should succeed")
}

fn build_run_id(issue_identifier: &str, attempt_number: i64) -> Result<String> {
	let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

	Ok(format!("{}-attempt-{attempt_number}-{timestamp}", issue_identifier.to_lowercase()))
}

fn resolve_config_path(explicit_path: Option<&Path>) -> Result<Option<PathBuf>> {
	if let Some(path) = explicit_path {
		return Ok(Some(path.to_path_buf()));
	}

	let repo_local = PathBuf::from("maestro.toml");

	if repo_local.exists() {
		return Ok(Some(repo_local));
	}

	let default_path = config::default_config_path()?;

	if default_path.exists() {
		return Ok(Some(default_path));
	}

	Ok(None)
}

fn default_state_store_path() -> Result<PathBuf> {
	let project_dirs = ProjectDirs::from("", "helixbox", env!("CARGO_PKG_NAME"))
		.ok_or_else(|| eyre::eyre!("Failed to resolve project directories."))?;

	Ok(project_dirs.data_dir().join("maestro.sqlite3"))
}

fn sleep_until_next_tick(poll_interval: Duration, tick_started_at: Instant) {
	let elapsed = tick_started_at.elapsed();

	if elapsed < poll_interval {
		thread::sleep(poll_interval - elapsed);
	}
}

#[cfg(test)]
mod tests {
	use std::{cell::RefCell, fs, path::Path, process::Command};

	use tempfile::TempDir;

	use crate::{
		config::ServiceConfig,
		orchestrator::{self, RunSummary},
		prelude::Result,
		state::StateStore,
		tracker::{
			IssueTracker, TrackerIssue, TrackerLabel, TrackerProject, TrackerState, TrackerTeam,
		},
		workflow::WorkflowDocument,
		workspace::WorkspaceSpec,
	};

	struct FakeTracker {
		listed_issues: Vec<TrackerIssue>,
		project_exists: bool,
		refresh_snapshots: RefCell<Vec<Vec<TrackerIssue>>>,
		comments: RefCell<Vec<String>>,
	}
	impl FakeTracker {
		fn new(issues: Vec<TrackerIssue>) -> Self {
			Self::with_refresh_snapshots_and_project(issues.clone(), vec![issues], true)
		}

		fn with_refresh_snapshots(
			listed_issues: Vec<TrackerIssue>,
			refresh_snapshots: Vec<Vec<TrackerIssue>>,
		) -> Self {
			Self::with_refresh_snapshots_and_project(listed_issues, refresh_snapshots, true)
		}

		fn with_refresh_snapshots_and_project(
			listed_issues: Vec<TrackerIssue>,
			refresh_snapshots: Vec<Vec<TrackerIssue>>,
			project_exists: bool,
		) -> Self {
			Self {
				listed_issues,
				project_exists,
				refresh_snapshots: RefCell::new(refresh_snapshots),
				comments: RefCell::new(Vec::new()),
			}
		}
	}
	impl IssueTracker for FakeTracker {
		fn list_project_issues(&self, _project_slug: &str) -> Result<Vec<TrackerIssue>> {
			Ok(self.listed_issues.clone())
		}

		fn get_project_by_slug(&self, project_slug: &str) -> Result<Option<TrackerProject>> {
			Ok(self.project_exists.then_some(TrackerProject {
				id: String::from("project-1"),
				name: String::from("Pubfi"),
				slug: project_slug.to_owned(),
			}))
		}

		fn refresh_issues(&self, issue_ids: &[String]) -> Result<Vec<TrackerIssue>> {
			let snapshot = {
				let mut refresh_snapshots = self.refresh_snapshots.borrow_mut();

				if refresh_snapshots.is_empty() {
					self.listed_issues.clone()
				} else {
					refresh_snapshots.remove(0)
				}
			};

			Ok(snapshot
				.iter()
				.filter(|issue| issue_ids.iter().any(|issue_id| issue_id == &issue.id))
				.cloned()
				.collect())
		}

		fn update_issue_state(&self, _issue_id: &str, _state_id: &str) -> Result<()> {
			Ok(())
		}

		fn update_issue_labels(&self, _issue_id: &str, _label_ids: &[String]) -> Result<()> {
			Ok(())
		}

		fn create_comment(&self, _issue_id: &str, body: &str) -> Result<()> {
			self.comments.borrow_mut().push(body.to_owned());

			Ok(())
		}
	}

	fn sample_issue(state_name: &str, labels: &[&str]) -> TrackerIssue {
		let team_labels = vec![
			TrackerLabel {
				id: String::from("label-manual"),
				name: String::from("maestro:manual-only"),
			},
			TrackerLabel {
				id: String::from("label-needs-attention"),
				name: String::from("maestro:needs-attention"),
			},
		];

		TrackerIssue {
			id: String::from("issue-1"),
			identifier: String::from("PUB-101"),
			title: String::from("Implement orchestration"),
			description: String::from("Body"),
			state: TrackerState { id: String::from("state-current"), name: state_name.to_owned() },
			team: TrackerTeam {
				id: String::from("team-1"),
				name: String::from("Pubfi"),
				states: vec![
					TrackerState { id: String::from("state-todo"), name: String::from("Todo") },
					TrackerState {
						id: String::from("state-progress"),
						name: String::from("In Progress"),
					},
					TrackerState {
						id: String::from("state-review"),
						name: String::from("In Review"),
					},
				],
				labels: team_labels.clone(),
			},
			labels: labels
				.iter()
				.enumerate()
				.map(|(index, label)| TrackerLabel {
					id: format!("label-{index}"),
					name: (*label).to_owned(),
				})
				.collect(),
		}
	}

	fn temp_project_layout() -> (TempDir, ServiceConfig, WorkflowDocument) {
		let temp_dir = TempDir::new().expect("temp dir should exist");
		let repo_root = temp_dir.path().join("target-repo");
		let workspace_root = temp_dir.path().join("workspaces");

		fs::create_dir_all(&repo_root).expect("repo root should exist");
		fs::create_dir_all(&workspace_root).expect("workspace root should exist");
		fs::write(repo_root.join("AGENTS.md"), "Read me first.\n").expect("AGENTS should exist");
		fs::write(repo_root.join("WORKFLOW.md"), sample_workflow_markdown())
			.expect("workflow should exist");

		assert!(
			Command::new("git")
				.arg("init")
				.arg("-b")
				.arg("main")
				.current_dir(&repo_root)
				.status()
				.expect("git init should run")
				.success()
		);
		assert!(
			Command::new("git")
				.args(["config", "user.name", "Maestro Tests"])
				.current_dir(&repo_root)
				.status()
				.expect("git config should run")
				.success()
		);
		assert!(
			Command::new("git")
				.args(["config", "user.email", "maestro-tests@example.com"])
				.current_dir(&repo_root)
				.status()
				.expect("git config should run")
				.success()
		);
		assert!(
			Command::new("git")
				.args(["add", "AGENTS.md", "WORKFLOW.md"])
				.current_dir(&repo_root)
				.status()
				.expect("git add should run")
				.success()
		);
		assert!(
			Command::new("git")
				.args(["commit", "-m", "bootstrap repo"])
				.current_dir(&repo_root)
				.status()
				.expect("git commit should run")
				.success()
		);

		let config = ServiceConfig::parse_toml(&format!(
			r#"
				id = "pubfi"
				repo_root = "{}"
				workspace_root = "{}"

				[tracker]
				project_slug = "pubfi"
				api_key = "lin_api_test"
			"#,
			repo_root.display(),
			workspace_root.display()
		))
		.expect("service config should parse");
		let workflow = WorkflowDocument::from_path(repo_root.join("WORKFLOW.md"))
			.expect("workflow should load");

		(temp_dir, config, workflow)
	}

	fn sample_workflow_markdown() -> &'static str {
		r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"
startable_states = ["Todo"]

[agent]
transport = "stdio://"
sandbox = "workspace-write"
approval_policy = "never"

[execution]
max_attempts = 3

[context]
read_first = ["AGENTS.md"]
+++

Follow the repository policy.
"#
	}

	#[test]
	fn eligibility_uses_state_label_and_lease_rules() {
		let (_, _, workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let eligible_issue = sample_issue("Todo", &[]);
		let opted_out_issue = sample_issue("Todo", &["maestro:manual-only"]);
		let wrong_state_issue = sample_issue("In Progress", &[]);

		assert!(
			orchestrator::is_issue_eligible(&eligible_issue, &workflow, &state_store)
				.expect("eligibility should succeed")
		);
		assert!(
			!orchestrator::is_issue_eligible(&opted_out_issue, &workflow, &state_store)
				.expect("eligibility should succeed")
		);
		assert!(
			!orchestrator::is_issue_eligible(&wrong_state_issue, &workflow, &state_store)
				.expect("eligibility should succeed")
		);

		state_store.upsert_lease("pubfi", "issue-1", "run-1").expect("lease should record");

		assert!(
			!orchestrator::is_issue_eligible(&eligible_issue, &workflow, &state_store)
				.expect("eligibility should succeed")
		);
	}

	#[test]
	fn dry_run_selects_one_issue_and_plans_workspace() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let tracker = FakeTracker::new(vec![sample_issue("Todo", &[])]);
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let summary =
			orchestrator::run_project_once(&tracker, &config, &workflow, &state_store, true)
				.expect("run once should succeed")
				.expect("one issue should be selected");

		assert_eq!(
			summary,
			RunSummary {
				project_id: String::from("pubfi"),
				issue_identifier: String::from("PUB-101"),
				branch_name: String::from("x/pubfi-pub-101"),
				worktree_path: Path::new(&config.workspace_root().join("PUB-101")).to_path_buf(),
				attempt_number: 1,
				run_id: summary.run_id.clone(),
			}
		);
		assert!(tracker.comments.borrow().is_empty());
	}

	#[test]
	fn failure_comments_use_repo_relative_worktree_paths() {
		let (_temp_dir, config, _workflow) = temp_project_layout();
		let workspace = WorkspaceSpec {
			branch_name: String::from("x/pubfi-pub-101"),
			issue_identifier: String::from("PUB-101"),
			path: config.repo_root().join(".worktrees/PUB-101"),
			reused_existing: true,
		};

		assert_eq!(orchestrator::relative_worktree_path(&config, &workspace), ".worktrees/PUB-101");
	}

	#[test]
	fn reconciliation_clears_stale_leases_and_terminal_worktrees() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let issue = sample_issue("Done", &[]);
		let tracker = FakeTracker::new(vec![issue.clone()]);
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let worktree_path = config.workspace_root().join("PUB-101");

		state_store
			.record_run_attempt("run-1", &issue.id, 1, "running")
			.expect("run attempt should record");
		state_store.upsert_lease("pubfi", &issue.id, "run-1").expect("lease should record");
		state_store
			.upsert_worktree(
				"pubfi",
				&issue.id,
				"x/pubfi-pub-101",
				&worktree_path.display().to_string(),
			)
			.expect("worktree mapping should record");

		let summary =
			orchestrator::run_project_once(&tracker, &config, &workflow, &state_store, false)
				.expect("reconciliation should succeed");

		assert!(summary.is_none());
		assert!(
			state_store.lease_for_issue(&issue.id).expect("lease lookup should work").is_none()
		);
		assert!(
			state_store
				.worktree_for_issue(&issue.id)
				.expect("worktree lookup should work")
				.is_none()
		);
		assert_eq!(
			state_store
				.run_attempt("run-1")
				.expect("run attempt lookup should work")
				.expect("run attempt should exist")
				.status(),
			"terminated"
		);
	}

	#[test]
	fn reconciliation_runs_before_project_validation() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let issue = sample_issue("Done", &[]);
		let tracker = FakeTracker::with_refresh_snapshots_and_project(
			vec![issue.clone()],
			vec![vec![issue.clone()]],
			false,
		);
		let state_store = StateStore::open_in_memory().expect("state store should open");

		state_store
			.record_run_attempt("run-1", &issue.id, 1, "running")
			.expect("run attempt should record");
		state_store.upsert_lease("pubfi", &issue.id, "run-1").expect("lease should record");

		let error =
			orchestrator::run_project_once(&tracker, &config, &workflow, &state_store, false)
				.expect_err("missing project validation should fail after reconciliation");

		assert!(error.to_string().contains("Linear project slug `pubfi` was not found."));
		assert!(
			state_store.lease_for_issue(&issue.id).expect("lease lookup should work").is_none()
		);
		assert_eq!(
			state_store
				.run_attempt("run-1")
				.expect("run attempt lookup should work")
				.expect("run attempt should exist")
				.status(),
			"terminated"
		);
	}

	#[test]
	fn live_run_skips_issue_that_becomes_ineligible_after_workspace_prepare() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let listed_issue = sample_issue("Todo", &[]);
		let tracker = FakeTracker::with_refresh_snapshots(
			vec![listed_issue.clone()],
			vec![vec![listed_issue.clone()], vec![sample_issue("In Progress", &[])]],
		);
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let summary =
			orchestrator::run_project_once(&tracker, &config, &workflow, &state_store, false)
				.expect("run once should succeed");

		assert!(summary.is_none());
		assert!(
			state_store
				.lease_for_issue(&listed_issue.id)
				.expect("lease lookup should work")
				.is_none()
		);
		assert!(
			state_store
				.worktree_for_issue(&listed_issue.id)
				.expect("worktree lookup should work")
				.is_some()
		);
		assert!(tracker.comments.borrow().is_empty());
	}
}
