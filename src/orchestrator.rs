use std::{
	cmp::Ordering,
	collections::{HashMap, HashSet},
	env,
	error::Error,
	fmt::{self, Display, Formatter},
	fs,
	path::{Path, PathBuf},
	process::{Child, Command, Stdio},
	slice, thread,
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use color_eyre::Report;
use directories::ProjectDirs;
use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
	agent::{
		self, ACTIVE_RUN_IDLE_TIMEOUT, AppServerRunRequest, ISSUE_COMMENT_TOOL_NAME,
		ISSUE_LABEL_ADD_TOOL_NAME, ISSUE_REVIEW_HANDOFF_TOOL_NAME, ISSUE_TRANSITION_TOOL_NAME,
		ReviewHandoffContext, ReviewHandoffWritebackFailed, RunCompletionDisposition,
		TrackerToolBridge,
	},
	config::{self, ServiceConfig},
	prelude::eyre,
	state::{ProjectRunStatus, RunAttempt, StateStore, WorkspaceMapping},
	tracker::{IssueTracker, TrackerIssue, linear::LinearClient},
	workflow::WorkflowDocument,
	workspace::{WorkspaceManager, WorkspaceSpec},
};

pub(crate) const DEFAULT_STATUS_RUN_LIMIT: usize = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RunSummary {
	project_id: String,
	issue_id: String,
	issue_identifier: String,
	branch_name: String,
	workspace_path: PathBuf,
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

#[derive(Debug)]
struct ManualAttentionRequested {
	issue_identifier: String,
	label: String,
	run_id: String,
}
impl Display for ManualAttentionRequested {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"Run `{}` for issue `{}` requested human attention via label `{}`; stop automatic retries and hand off manually.",
			self.run_id, self.issue_identifier, self.label
		)
	}
}

impl Error for ManualAttentionRequested {}

#[derive(Debug)]
struct ReviewHandoffNeedsAttention {
	issue_identifier: String,
	run_id: String,
}
impl Display for ReviewHandoffNeedsAttention {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"Run `{}` for issue `{}` partially applied review handoff writeback; stop retries and repair the issue manually.",
			self.run_id, self.issue_identifier
		)
	}
}

impl Error for ReviewHandoffNeedsAttention {}

#[derive(Debug)]
struct StalledRunNeedsAttention {
	issue_identifier: String,
	run_id: String,
	idle_for: Duration,
}
impl Display for StalledRunNeedsAttention {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"Run `{}` for issue `{}` stalled after {:?} without app-server activity; stop automatic execution and repair manually.",
			self.run_id, self.issue_identifier, self.idle_for
		)
	}
}

impl Error for StalledRunNeedsAttention {}

struct DaemonRunChild {
	child: Child,
	issue_id: String,
	run_id: String,
}

struct DaemonTickContext {
	config: ServiceConfig,
	workflow: WorkflowDocument,
	tracker: LinearClient,
	workspace_manager: WorkspaceManager,
}

#[derive(Clone, Debug)]
enum ActiveRunDisposition {
	Terminal,
	NonActive,
	Stalled { idle_for: Duration },
}

#[derive(Clone, Debug)]
struct ActiveRunReconciliation {
	issue: TrackerIssue,
	run_attempt: RunAttempt,
	workspace_mapping: Option<WorkspaceMapping>,
	disposition: ActiveRunDisposition,
}

struct TerminalFailureOutcome {
	error_class: &'static str,
}

#[derive(Debug, Serialize)]
struct OperatorStatusSnapshot {
	project_id: String,
	run_limit: usize,
	active_runs: Vec<OperatorRunStatus>,
	recent_runs: Vec<OperatorRunStatus>,
	workspaces: Vec<OperatorWorkspaceStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct OperatorRunStatus {
	run_id: String,
	issue_id: String,
	attempt_number: i64,
	status: String,
	thread_id: Option<String>,
	active_lease: bool,
	updated_at: String,
	last_event_type: Option<String>,
	last_event_at: Option<String>,
	event_count: i64,
	branch_name: Option<String>,
	workspace_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct OperatorWorkspaceStatus {
	issue_id: String,
	branch_name: String,
	workspace_path: String,
}

pub(crate) fn run_once(config_path: Option<&Path>, dry_run: bool) -> crate::prelude::Result<()> {
	let Some(config_path) = resolve_config_path(config_path)? else {
		if dry_run {
			println!("dry run: no maestro config found; nothing to execute.");

			return Ok(());
		}

		eyre::bail!("No maestro config found. Pass --config or create tmp/maestro.toml.");
	};
	let state_store = if dry_run {
		StateStore::open_in_memory()?
	} else {
		StateStore::open(default_state_store_path()?)?
	};

	if let Some(summary) = run_configured_cycle(&config_path, &state_store, dry_run)? {
		if dry_run {
			println!(
				"dry run: project={} issue={} branch={} workspace={} attempt={}",
				summary.project_id,
				summary.issue_identifier,
				summary.branch_name,
				summary.workspace_path.display(),
				summary.attempt_number
			);
		} else {
			println!(
				"run complete: project={} issue={} run_id={} workspace={}",
				summary.project_id,
				summary.issue_identifier,
				summary.run_id,
				summary.workspace_path.display()
			);
		}

		return Ok(());
	}

	println!("No eligible issue found for the configured project.");

	Ok(())
}

pub(crate) fn run_daemon(
	config_path: Option<&Path>,
	poll_interval: Duration,
) -> crate::prelude::Result<()> {
	if poll_interval.is_zero() {
		eyre::bail!("`daemon --poll-interval-s` must be greater than zero.");
	}

	let Some(config_path) = resolve_config_path(config_path)? else {
		eyre::bail!("No maestro config found. Pass --config or create tmp/maestro.toml.");
	};
	let state_store = StateStore::open(default_state_store_path()?)?;
	let mut active_child: Option<DaemonRunChild> = None;

	tracing::info!(
		config_path = %config_path.display(),
		poll_interval_s = poll_interval.as_secs(),
		"Starting daemon poll loop."
	);

	loop {
		let tick_started_at = Instant::now();

		match load_daemon_tick_context(&config_path).and_then(|context| {
			run_daemon_tick(&config_path, &state_store, &mut active_child, context)
		}) {
			Ok(()) => {},
			Err(error) => {
				tracing::warn!(?error, "Daemon tick failed.");
			},
		}

		sleep_until_next_tick(poll_interval, tick_started_at);
	}
}

pub(crate) fn print_status(
	config_path: Option<&Path>,
	json: bool,
	limit: usize,
) -> crate::prelude::Result<()> {
	if limit == 0 {
		eyre::bail!("`status --limit` must be greater than zero.");
	}

	let Some(config_path) = resolve_config_path(config_path)? else {
		eyre::bail!("No maestro config found. Pass --config or create tmp/maestro.toml.");
	};
	let config = ServiceConfig::from_path(&config_path)?;
	let state_store = StateStore::open(default_state_store_path()?)?;
	let snapshot = build_operator_status_snapshot(&config, &state_store, limit)?;

	if json {
		println!("{}", serde_json::to_string_pretty(&snapshot)?);
	} else {
		print!("{}", render_operator_status(&snapshot));
	}

	Ok(())
}

fn load_daemon_tick_context(config_path: &Path) -> crate::prelude::Result<DaemonTickContext> {
	let config = ServiceConfig::from_path(config_path)?;
	let workflow_path = config.repo_root().join(config.workflow_path());
	let workflow = WorkflowDocument::from_path(&workflow_path)?;
	let api_key = config.tracker().resolve_api_key()?;
	let tracker = LinearClient::new(api_key)?;
	let workspace_manager =
		WorkspaceManager::new(config.id(), config.repo_root(), config.workspace_root());

	Ok(DaemonTickContext { config, workflow, tracker, workspace_manager })
}

fn run_daemon_tick(
	config_path: &Path,
	state_store: &StateStore,
	active_child: &mut Option<DaemonRunChild>,
	context: DaemonTickContext,
) -> crate::prelude::Result<()> {
	inspect_or_clear_active_child(
		active_child,
		&context.tracker,
		&context.config,
		&context.workflow,
		state_store,
		&context.workspace_manager,
	)?;

	if active_child.is_none() {
		reconcile_project_state(
			&context.tracker,
			&context.config,
			&context.workflow,
			state_store,
			&context.workspace_manager,
		)?;
		validate_project_contract(&context.config, &context.workflow)?;
		validate_tracker_project(&context.tracker, context.config.tracker().project_slug())?;
		spawn_next_daemon_child(
			config_path,
			state_store,
			active_child,
			&context.tracker,
			&context.config,
			&context.workflow,
		)?;
	}

	Ok(())
}

fn inspect_or_clear_active_child<T>(
	active_child: &mut Option<DaemonRunChild>,
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	workspace_manager: &WorkspaceManager,
) -> crate::prelude::Result<()>
where
	T: IssueTracker,
{
	let Some(daemon_child) = active_child.as_mut() else {
		return Ok(());
	};
	let child_exited = daemon_child.child.try_wait()?.is_some();
	let actions = if child_exited {
		inspect_exited_daemon_child_reconciliation(
			tracker,
			project,
			workflow,
			state_store,
			&daemon_child.issue_id,
			&daemon_child.run_id,
		)?
	} else {
		inspect_active_run_reconciliation(tracker, project, workflow, state_store)?
	};

	if actions.is_empty() {
		if child_exited {
			clear_orphaned_daemon_child_state(
				state_store,
				&daemon_child.issue_id,
				&daemon_child.run_id,
			)?;

			active_child.take();
		}

		return Ok(());
	}
	if !child_exited {
		stop_daemon_child(&mut daemon_child.child)?;
	}

	apply_active_run_reconciliation(
		tracker,
		project,
		workflow,
		state_store,
		workspace_manager,
		actions,
	)?;

	active_child.take();

	Ok(())
}

fn clear_orphaned_daemon_child_state(
	state_store: &StateStore,
	issue_id: &str,
	run_id: &str,
) -> crate::prelude::Result<()> {
	mark_run_attempt_if_active(state_store, run_id, "interrupted")?;

	let lease_matches_run =
		state_store.lease_for_issue(issue_id)?.is_some_and(|lease| lease.run_id() == run_id);

	if lease_matches_run {
		state_store.clear_lease(issue_id)?;
	}

	Ok(())
}

fn spawn_next_daemon_child<T>(
	config_path: &Path,
	state_store: &StateStore,
	active_child: &mut Option<DaemonRunChild>,
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
) -> crate::prelude::Result<()>
where
	T: IssueTracker,
{
	match run_project_once(tracker, project, workflow, state_store, true)? {
		Some(summary) => {
			validate_review_handoff_runtime(false)?;

			let child = spawn_run_once_child(config_path)?;

			tracing::info!(
				issue = summary.issue_identifier,
				workspace = %summary.workspace_path.display(),
				"Spawned daemon child for active issue lane."
			);

			*active_child =
				Some(DaemonRunChild { child, issue_id: summary.issue_id, run_id: summary.run_id });
		},
		None => {
			tracing::debug!("Daemon tick found no eligible issue.");
		},
	}

	Ok(())
}

fn spawn_run_once_child(config_path: &Path) -> crate::prelude::Result<Child> {
	let executable = env::current_exe()?;
	let child = Command::new(executable)
		.args(["run", "--once", "--config"])
		.arg(config_path)
		.stdin(Stdio::null())
		.stdout(Stdio::inherit())
		.stderr(Stdio::inherit())
		.spawn()?;

	Ok(child)
}

fn stop_daemon_child(child: &mut Child) -> crate::prelude::Result<()> {
	if child.try_wait()?.is_some() {
		return Ok(());
	}

	let _ = child.kill();
	let _ = child.wait();

	Ok(())
}

fn inspect_active_run_reconciliation<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
) -> crate::prelude::Result<Vec<ActiveRunReconciliation>>
where
	T: IssueTracker,
{
	inspect_active_run_reconciliation_at(
		tracker,
		project,
		workflow,
		state_store,
		OffsetDateTime::now_utc().unix_timestamp(),
	)
}

fn inspect_active_run_reconciliation_at<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	now_unix_epoch: i64,
) -> crate::prelude::Result<Vec<ActiveRunReconciliation>>
where
	T: IssueTracker,
{
	let leases = state_store.list_leases(project.id())?;

	if leases.is_empty() {
		return Ok(Vec::new());
	}

	let issue_ids = leases.iter().map(|lease| lease.issue_id().to_owned()).collect::<Vec<_>>();
	let issues = tracker.refresh_issues(&issue_ids)?;
	let issues_by_id =
		issues.into_iter().map(|issue| (issue.id.clone(), issue)).collect::<HashMap<_, _>>();
	let mut actions = Vec::new();

	for lease in leases {
		let Some(issue) = issues_by_id.get(lease.issue_id()).cloned() else {
			continue;
		};
		let Some(run_attempt) = state_store.run_attempt(lease.run_id())? else {
			continue;
		};
		let disposition = if is_terminal_issue(&issue, workflow) {
			Some(ActiveRunDisposition::Terminal)
		} else if is_issue_nonactive_for_run(&issue, workflow) {
			Some(ActiveRunDisposition::NonActive)
		} else {
			stalled_idle_duration(state_store, &run_attempt, now_unix_epoch)?
				.map(|idle_for| ActiveRunDisposition::Stalled { idle_for })
		};

		if let Some(disposition) = disposition {
			actions.push(ActiveRunReconciliation {
				issue: issue.clone(),
				run_attempt,
				workspace_mapping: state_store.workspace_for_issue(&issue.id)?,
				disposition,
			});
		}
	}

	Ok(actions)
}

fn inspect_exited_daemon_child_reconciliation<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	issue_id: &str,
	run_id: &str,
) -> crate::prelude::Result<Vec<ActiveRunReconciliation>>
where
	T: IssueTracker,
{
	inspect_exited_daemon_child_reconciliation_at(
		tracker,
		project,
		workflow,
		state_store,
		issue_id,
		run_id,
		OffsetDateTime::now_utc().unix_timestamp(),
	)
}

fn inspect_exited_daemon_child_reconciliation_at<T>(
	tracker: &T,
	_project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	issue_id: &str,
	run_id: &str,
	now_unix_epoch: i64,
) -> crate::prelude::Result<Vec<ActiveRunReconciliation>>
where
	T: IssueTracker,
{
	let Some(issue) = refresh_issue(tracker, issue_id)? else {
		return Ok(Vec::new());
	};
	let Some(run_attempt) = state_store.run_attempt(run_id)? else {
		return Ok(Vec::new());
	};

	if run_attempt.status() != "failed" || !is_issue_active_for_run(&issue, workflow) {
		return Ok(Vec::new());
	}

	let Some(idle_for) = stalled_protocol_idle_duration(state_store, run_id, now_unix_epoch)?
	else {
		return Ok(Vec::new());
	};

	Ok(vec![ActiveRunReconciliation {
		issue,
		run_attempt,
		workspace_mapping: state_store.workspace_for_issue(issue_id)?,
		disposition: ActiveRunDisposition::Stalled { idle_for },
	}])
}

fn apply_active_run_reconciliation<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	workspace_manager: &WorkspaceManager,
	actions: Vec<ActiveRunReconciliation>,
) -> crate::prelude::Result<()>
where
	T: IssueTracker,
{
	for action in actions {
		match action.disposition {
			ActiveRunDisposition::Terminal => {
				tracing::info!(
					project_id = project.id(),
					issue_id = action.issue.id,
					issue = action.issue.identifier,
					run_id = action.run_attempt.run_id(),
					disposition = "terminal",
					"Reconciling terminal active run."
				);

				mark_run_attempt_if_active(state_store, action.run_attempt.run_id(), "terminated")?;

				state_store.clear_lease(&action.issue.id)?;

				if let Some(mapping) = &action.workspace_mapping {
					cleanup_workspace_mapping(state_store, workspace_manager, mapping)?;
				}
			},
			ActiveRunDisposition::NonActive => {
				tracing::info!(
					project_id = project.id(),
					issue_id = action.issue.id,
					issue = action.issue.identifier,
					run_id = action.run_attempt.run_id(),
					disposition = "non_active",
					"Reconciling non-active run."
				);

				mark_run_attempt_if_active(
					state_store,
					action.run_attempt.run_id(),
					"interrupted",
				)?;

				state_store.clear_lease(&action.issue.id)?;
			},
			ActiveRunDisposition::Stalled { idle_for } => {
				tracing::warn!(
					project_id = project.id(),
					issue_id = action.issue.id,
					issue = action.issue.identifier,
					run_id = action.run_attempt.run_id(),
					disposition = "stalled",
					idle_for_s = idle_for.as_secs(),
					"Reconciling stalled run."
				);

				state_store.update_run_status(action.run_attempt.run_id(), "stalled")?;
				state_store.clear_lease(&action.issue.id)?;

				let workspace = action.workspace_mapping.as_ref().map_or_else(
					|| workspace_manager.plan_for_issue(&action.issue.identifier),
					|mapping| WorkspaceSpec {
						branch_name: mapping.branch_name().to_owned(),
						issue_identifier: action.issue.identifier.clone(),
						path: mapping.workspace_path().to_path_buf(),
						reused_existing: true,
					},
				);
				let issue_run = IssueRunPlan {
					issue: action.issue.clone(),
					workspace,
					attempt_number: action.run_attempt.attempt_number(),
					run_id: action.run_attempt.run_id().to_owned(),
				};

				handle_failure(
					tracker,
					project,
					workflow,
					&issue_run,
					&Report::new(StalledRunNeedsAttention {
						issue_identifier: action.issue.identifier.clone(),
						run_id: action.run_attempt.run_id().to_owned(),
						idle_for,
					}),
				)?;
			},
		}
	}

	Ok(())
}

fn stalled_idle_duration(
	state_store: &StateStore,
	run_attempt: &RunAttempt,
	now_unix_epoch: i64,
) -> crate::prelude::Result<Option<Duration>> {
	if !matches!(run_attempt.status(), "starting" | "running") {
		return Ok(None);
	}

	let Some(last_activity) = state_store.last_run_activity_unix_epoch(run_attempt.run_id())?
	else {
		return Ok(None);
	};
	let Some(idle_for) = observed_idle_duration(last_activity, now_unix_epoch) else {
		return Ok(None);
	};

	if idle_for >= ACTIVE_RUN_IDLE_TIMEOUT {
		return Ok(Some(idle_for));
	}

	Ok(None)
}

fn stalled_protocol_idle_duration(
	state_store: &StateStore,
	run_id: &str,
	now_unix_epoch: i64,
) -> crate::prelude::Result<Option<Duration>> {
	let Some(last_activity) = state_store.last_protocol_activity_unix_epoch(run_id)? else {
		return Ok(None);
	};
	let Some(idle_for) = observed_idle_duration(last_activity, now_unix_epoch) else {
		return Ok(None);
	};

	if idle_for >= ACTIVE_RUN_IDLE_TIMEOUT {
		return Ok(Some(idle_for));
	}

	Ok(None)
}

fn observed_idle_duration(last_activity_unix_epoch: i64, now_unix_epoch: i64) -> Option<Duration> {
	now_unix_epoch
		.checked_sub(last_activity_unix_epoch)
		.and_then(|idle_seconds| u64::try_from(idle_seconds).ok())
		.map(Duration::from_secs)
}

fn run_configured_cycle(
	config_path: &Path,
	state_store: &StateStore,
	dry_run: bool,
) -> crate::prelude::Result<Option<RunSummary>> {
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
) -> crate::prelude::Result<Option<RunSummary>>
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
	validate_review_handoff_runtime(dry_run)?;

	let project_slug = project.tracker().project_slug();
	let issues = tracker.list_project_issues(project_slug)?;
	let Some(issue) = select_issue_candidate(issues, workflow, state_store, project.id())? else {
		return Ok(None);
	};
	let mut refreshed_issues = tracker.refresh_issues(slice::from_ref(&issue.id))?;
	let Some(issue) = refreshed_issues.pop() else {
		return Ok(None);
	};

	if !has_available_dispatch_slot(project.id(), state_store)? {
		return Ok(None);
	}
	if !is_issue_eligible(&issue, workflow, state_store)? {
		return Ok(None);
	}

	let attempt_number = state_store.next_attempt_number(&issue.id)?;
	let run_id = build_run_id(&issue.identifier, attempt_number)?;
	let lease_issue_id = issue.id.clone();

	if !dry_run && !state_store.try_acquire_lease(project.id(), &issue.id, &run_id)? {
		return Ok(None);
	}

	let issue_run = match (|| -> crate::prelude::Result<Option<IssueRunPlan>> {
		let workspace = workspace_manager.ensure_workspace(&issue.identifier, dry_run)?;

		if !dry_run {
			state_store.upsert_workspace(
				project.id(),
				&lease_issue_id,
				&workspace.branch_name,
				&workspace.path.display().to_string(),
			)?;
		}

		let Some(refreshed_issue) = refresh_issue(tracker, &lease_issue_id)? else {
			return Ok(None);
		};

		if !is_issue_eligible(&refreshed_issue, workflow, state_store)? {
			if !dry_run && is_terminal_issue(&refreshed_issue, workflow) {
				cleanup_terminal_workspace(
					state_store,
					&workspace_manager,
					&lease_issue_id,
					&workspace.path,
				)?;
			}

			return Ok(None);
		}

		Ok(Some(IssueRunPlan {
			issue: refreshed_issue,
			workspace,
			attempt_number,
			run_id: run_id.clone(),
		}))
	})() {
		Ok(Some(issue_run)) => issue_run,
		Ok(None) => {
			if !dry_run {
				state_store.clear_lease(&lease_issue_id)?;
			}

			return Ok(None);
		},
		Err(error) => {
			if !dry_run {
				state_store.clear_lease(&lease_issue_id)?;
			}

			return Err(error);
		},
	};

	if dry_run {
		return Ok(Some(RunSummary {
			project_id: project.id().to_owned(),
			issue_id: issue_run.issue.id.clone(),
			issue_identifier: issue_run.issue.identifier.clone(),
			branch_name: issue_run.workspace.branch_name.clone(),
			workspace_path: issue_run.workspace.path.clone(),
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
) -> crate::prelude::Result<()>
where
	T: IssueTracker,
{
	let leases = state_store.list_leases(project.id())?;
	let workspaces = state_store.list_workspaces(project.id())?;

	if leases.is_empty() && workspaces.is_empty() {
		return Ok(());
	}

	let mut issue_ids = HashSet::new();

	for lease in &leases {
		issue_ids.insert(lease.issue_id().to_owned());
	}
	for mapping in &workspaces {
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
	for mapping in &workspaces {
		if issues_by_id
			.get(mapping.issue_id())
			.is_some_and(|issue| is_terminal_issue(issue, workflow))
		{
			cleanup_workspace_mapping(state_store, workspace_manager, mapping)?;
		}
	}

	Ok(())
}

fn validate_tracker_project<T>(tracker: &T, project_slug: &str) -> crate::prelude::Result<()>
where
	T: IssueTracker,
{
	tracker
		.get_project_by_slug(project_slug)?
		.ok_or_else(|| eyre::eyre!("Linear project slug `{project_slug}` was not found."))?;

	Ok(())
}

fn validate_review_handoff_runtime(dry_run: bool) -> crate::prelude::Result<()> {
	if dry_run {
		return Ok(());
	}

	validate_command_available("gh", "PR-backed review handoff")?;

	Ok(())
}

fn validate_command_available(command: &str, purpose: &str) -> crate::prelude::Result<()> {
	let output = Command::new(command).arg("--version").output().map_err(|error| {
		eyre::eyre!("Required command `{command}` is unavailable for {purpose}: {error}")
	})?;

	if output.status.success() {
		return Ok(());
	}

	let stderr = String::from_utf8_lossy(&output.stderr);
	let stdout = String::from_utf8_lossy(&output.stdout);
	let detail = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };

	if detail.is_empty() {
		eyre::bail!(
			"Required command `{command}` is unavailable for {purpose}: `{command} --version` exited unsuccessfully."
		);
	}

	eyre::bail!(
		"Required command `{command}` is unavailable for {purpose}: `{command} --version` failed with `{detail}`."
	);
}

fn execute_issue_run<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	issue_run: IssueRunPlan,
) -> crate::prelude::Result<RunSummary>
where
	T: IssueTracker,
{
	tracing::info!(
		project_id = project.id(),
		issue_id = issue_run.issue.id,
		issue = issue_run.issue.identifier,
		run_id = issue_run.run_id,
		attempt = issue_run.attempt_number,
		branch = issue_run.workspace.branch_name,
		workspace_path = %relative_workspace_path(project, &issue_run.workspace),
		"Starting issue run."
	);

	state_store.upsert_workspace(
		project.id(),
		&issue_run.issue.id,
		&issue_run.workspace.branch_name,
		&issue_run.workspace.path.display().to_string(),
	)?;

	let result = execute_issue_run_inner(tracker, project, workflow, state_store, &issue_run);

	state_store.clear_lease(&issue_run.issue.id)?;

	match result {
		Ok(summary) => {
			persist_issue_run_outcome(state_store, &issue_run.run_id, true)?;

			tracing::info!(
				project_id = project.id(),
				issue_id = issue_run.issue.id,
				issue = issue_run.issue.identifier,
				run_id = issue_run.run_id,
				attempt = issue_run.attempt_number,
				branch = issue_run.workspace.branch_name,
				workspace_path = %relative_workspace_path(project, &issue_run.workspace),
				"Completed issue run."
			);

			Ok(summary)
		},
		Err(error) => {
			persist_issue_run_outcome(state_store, &issue_run.run_id, false)?;
			handle_failure(tracker, project, workflow, &issue_run, &error)?;

			Err(error)
		},
	}
}

fn persist_issue_run_outcome(
	state_store: &StateStore,
	run_id: &str,
	succeeded: bool,
) -> crate::prelude::Result<()> {
	state_store.update_run_status(run_id, if succeeded { "succeeded" } else { "failed" })
}

fn execute_issue_run_inner<T>(
	tracker: &T,
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	issue_run: &IssueRunPlan,
) -> crate::prelude::Result<RunSummary>
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
	let tracker_tool_bridge = TrackerToolBridge::with_run_context(
		tracker,
		&issue_run.issue,
		workflow,
		ReviewHandoffContext {
			attempt_number: issue_run.attempt_number,
			branch_name: issue_run.workspace.branch_name.clone(),
			run_id: issue_run.run_id.clone(),
			workspace_path: relative_workspace_path(project, &issue_run.workspace),
			cwd: issue_run.workspace.path.clone(),
		},
	);

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
			timeout: ACTIVE_RUN_IDLE_TIMEOUT,
			dynamic_tool_handler: Some(&tracker_tool_bridge),
		},
		state_store,
	)?;

	match tracker_tool_bridge.completion_disposition()? {
		RunCompletionDisposition::ReviewHandoff => {
			run_validation_commands(
				workflow.frontmatter().execution().validation_commands(),
				&issue_run.workspace.path,
			)?;

			tracker_tool_bridge.apply_review_handoff().map_err(|error| {
				if let Some(writeback_error) = error.downcast_ref::<ReviewHandoffWritebackFailed>()
				{
					Report::new(ReviewHandoffNeedsAttention {
						issue_identifier: writeback_error.issue_identifier.clone(),
						run_id: writeback_error.run_id.clone(),
					})
					.wrap_err(error)
				} else {
					error
				}
			})?;
		},
		RunCompletionDisposition::ManualAttention => {
			return Err(Report::new(ManualAttentionRequested {
				issue_identifier: issue_run.issue.identifier.clone(),
				label: workflow.frontmatter().tracker().needs_attention_label().to_owned(),
				run_id: issue_run.run_id.clone(),
			}));
		},
	}

	Ok(RunSummary {
		project_id: project.id().to_owned(),
		issue_id: issue_run.issue.id.clone(),
		issue_identifier: issue_run.issue.identifier.clone(),
		branch_name: issue_run.workspace.branch_name.clone(),
		workspace_path: issue_run.workspace.path.clone(),
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
) -> crate::prelude::Result<()>
where
	T: IssueTracker,
{
	let max_attempts = i64::from(workflow.frontmatter().execution().max_attempts());
	let manual_attention_requested = error.downcast_ref::<ManualAttentionRequested>().is_some();
	let review_handoff_needs_attention =
		error.downcast_ref::<ReviewHandoffNeedsAttention>().is_some();
	let stalled_run_needs_attention = error.downcast_ref::<StalledRunNeedsAttention>().is_some();
	let workspace_path = relative_workspace_path(project, &issue_run.workspace);

	if !manual_attention_requested
		&& !review_handoff_needs_attention
		&& !stalled_run_needs_attention
		&& issue_run.attempt_number < max_attempts
	{
		tracing::warn!(
			project_id = project.id(),
			issue_id = issue_run.issue.id,
			issue = issue_run.issue.identifier,
			run_id = issue_run.run_id,
			attempt = issue_run.attempt_number,
			max_attempts,
			branch = issue_run.workspace.branch_name,
			workspace_path = %workspace_path,
			error_class = "retryable_execution_failure",
			"Run failed and remains retryable."
		);

		tracker.create_comment(
			&issue_run.issue.id,
			&format_retry_comment(
				&issue_run.run_id,
				issue_run.attempt_number,
				max_attempts,
				workspace_path,
				&issue_run.workspace.branch_name,
				error,
			),
		)?;

		return Ok(());
	}

	let outcome = apply_terminal_failure_writeback(
		tracker,
		workflow,
		issue_run,
		&workspace_path,
		manual_attention_requested,
		error,
	)?;

	tracing::warn!(
		project_id = project.id(),
		issue_id = issue_run.issue.id,
		issue = issue_run.issue.identifier,
		run_id = issue_run.run_id,
		attempt = issue_run.attempt_number,
		branch = issue_run.workspace.branch_name,
		workspace_path = %workspace_path,
		error_class = outcome.error_class,
		"Run failed and now requires operator attention."
	);

	Ok(())
}

fn apply_terminal_failure_writeback<T>(
	tracker: &T,
	workflow: &WorkflowDocument,
	issue_run: &IssueRunPlan,
	workspace_path: &str,
	manual_attention_requested: bool,
	error: &Report,
) -> crate::prelude::Result<TerminalFailureOutcome>
where
	T: IssueTracker,
{
	let tracker_policy = workflow.frontmatter().tracker();
	let needs_attention_label = tracker_policy.needs_attention_label();
	let needs_attention_label_id = issue_run.issue.label_id_for_name(needs_attention_label);
	let failure_state_name = tracker_policy.failure_state();
	let failure_state_is_startable =
		tracker_policy.startable_states().iter().any(|state| state == failure_state_name);
	let guard_with_nonstartable_state =
		needs_attention_label_id.is_none() && failure_state_is_startable;
	let terminal_failure_state_name = if guard_with_nonstartable_state {
		tracker_policy.in_progress_state()
	} else {
		failure_state_name
	};
	let failure_state_id =
		issue_run.issue.state_id_for_name(terminal_failure_state_name).ok_or_else(|| {
			eyre::eyre!(
				"State `{}` was not found for issue `{}`.",
				terminal_failure_state_name,
				issue_run.issue.identifier
			)
		})?;

	tracker.update_issue_state(&issue_run.issue.id, failure_state_id)?;

	let needs_attention_label_available = apply_needs_attention_label(
		tracker,
		issue_run,
		needs_attention_label,
		needs_attention_label_id,
		terminal_failure_state_name,
	)?;
	let recovery_gate = terminal_failure_recovery_gate(
		needs_attention_label,
		needs_attention_label_available,
		guard_with_nonstartable_state,
		tracker_policy.in_progress_state(),
	);
	let error_class = terminal_failure_error_class(manual_attention_requested, error);

	tracker.create_comment(
		&issue_run.issue.id,
		&format_terminal_failure_comment(
			&issue_run.run_id,
			issue_run.attempt_number,
			workspace_path.to_owned(),
			&issue_run.workspace.branch_name,
			&recovery_gate,
			manual_attention_requested,
			error,
		),
	)?;

	Ok(TerminalFailureOutcome { error_class })
}

fn apply_needs_attention_label<T>(
	tracker: &T,
	issue_run: &IssueRunPlan,
	needs_attention_label: &str,
	needs_attention_label_id: Option<&str>,
	terminal_failure_state_name: &str,
) -> crate::prelude::Result<bool>
where
	T: IssueTracker,
{
	if let Some(label_id) = needs_attention_label_id {
		let mut label_ids =
			issue_run.issue.labels.iter().map(|label| label.id.clone()).collect::<Vec<_>>();

		if !label_ids.iter().any(|existing| existing == label_id) {
			label_ids.push(label_id.to_owned());
			tracker.update_issue_labels(&issue_run.issue.id, &label_ids)?;
		}

		return Ok(true);
	}

	tracing::warn!(
		label = needs_attention_label,
		issue = issue_run.issue.identifier,
		guard_state = terminal_failure_state_name,
		"Needs-attention label was not found in the issue team; using a non-startable state guard when needed."
	);

	Ok(false)
}

fn terminal_failure_error_class(manual_attention_requested: bool, error: &Report) -> &'static str {
	if manual_attention_requested {
		"human_attention_required"
	} else if error.downcast_ref::<ReviewHandoffNeedsAttention>().is_some() {
		"review_handoff_writeback_failed"
	} else if error.downcast_ref::<StalledRunNeedsAttention>().is_some() {
		"stalled_run_detected"
	} else {
		"retry_budget_exhausted"
	}
}

fn validate_project_contract(
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
) -> crate::prelude::Result<()> {
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
) -> crate::prelude::Result<bool> {
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
	if issue.has_label(tracker_policy.needs_attention_label()) {
		return Ok(false);
	}
	if !todo_blocker_rule_passes(issue, workflow) {
		return Ok(false);
	}

	Ok(state_store.lease_for_issue(&issue.id)?.is_none())
}

fn has_available_dispatch_slot(
	project_id: &str,
	state_store: &StateStore,
) -> crate::prelude::Result<bool> {
	Ok(state_store.list_leases(project_id)?.is_empty())
}

fn todo_blocker_rule_passes(issue: &TrackerIssue, workflow: &WorkflowDocument) -> bool {
	if issue.state.name != "Todo" {
		return true;
	}

	issue.blockers.iter().all(|blocker| state_name_is_terminal(&blocker.state.name, workflow))
}

fn refresh_issue<T>(tracker: &T, issue_id: &str) -> crate::prelude::Result<Option<TrackerIssue>>
where
	T: IssueTracker,
{
	let issue_ids = [issue_id.to_owned()];
	let mut refreshed_issues = tracker.refresh_issues(&issue_ids)?;

	Ok(refreshed_issues.pop())
}

fn is_terminal_issue(issue: &TrackerIssue, workflow: &WorkflowDocument) -> bool {
	state_name_is_terminal(&issue.state.name, workflow)
}

fn state_name_is_terminal(state_name: &str, workflow: &WorkflowDocument) -> bool {
	workflow.frontmatter().tracker().terminal_states().iter().any(|state| state == state_name)
}

fn is_issue_active_for_run(issue: &TrackerIssue, workflow: &WorkflowDocument) -> bool {
	let tracker_policy = workflow.frontmatter().tracker();

	issue.state.name == tracker_policy.in_progress_state()
		&& !issue.has_label(tracker_policy.needs_attention_label())
}

fn is_issue_nonactive_for_run(issue: &TrackerIssue, workflow: &WorkflowDocument) -> bool {
	let tracker_policy = workflow.frontmatter().tracker();

	issue.has_label(tracker_policy.opt_out_label())
		|| issue.has_label(tracker_policy.needs_attention_label())
		|| (issue.state.name != tracker_policy.in_progress_state()
			&& !tracker_policy.startable_states().iter().any(|state| state == &issue.state.name))
}

fn mark_run_attempt_if_active(
	state_store: &StateStore,
	run_id: &str,
	reconciled_status: &str,
) -> crate::prelude::Result<()> {
	let Some(run_attempt) = state_store.run_attempt(run_id)? else {
		return Ok(());
	};

	if matches!(run_attempt.status(), "starting" | "running") {
		state_store.update_run_status(run_id, reconciled_status)?;
	}

	Ok(())
}

fn cleanup_workspace_mapping(
	state_store: &StateStore,
	workspace_manager: &WorkspaceManager,
	mapping: &WorkspaceMapping,
) -> crate::prelude::Result<()> {
	workspace_manager.remove_workspace_path(mapping.workspace_path())?;
	state_store.clear_workspace(mapping.issue_id())?;

	Ok(())
}

fn cleanup_terminal_workspace(
	state_store: &StateStore,
	workspace_manager: &WorkspaceManager,
	issue_id: &str,
	workspace_path: &Path,
) -> crate::prelude::Result<()> {
	workspace_manager.remove_workspace_path(workspace_path)?;
	state_store.clear_workspace(issue_id)?;

	Ok(())
}

fn build_developer_instructions(
	project: &ServiceConfig,
	workflow: &WorkflowDocument,
	issue_run: &IssueRunPlan,
) -> crate::prelude::Result<String> {
	let mut sections = Vec::new();

	for relative_path in workflow.frontmatter().context().read_first() {
		let absolute_path = project.repo_root().join(relative_path);
		let contents = fs::read_to_string(&absolute_path)?;

		sections.push(format!("File: {relative_path}\n{contents}"));
	}

	sections.push(String::from(
		"Execution discipline\n- Keep pre-edit discovery bounded to the smallest code surface that can satisfy the current issue.\n- Start with the implementation files directly implicated by the issue before reading broader docs or repo-wide guidance.\n- Do not browse upstream references or general repository documentation unless a concrete ambiguity blocks the change.\n- Once the relevant change surface is identified, patch code and run validation instead of continuing broad searches.",
	));

	sections.push(format!(
		"Tracker tool contract\n- You own issue-scoped tracker writes for `{issue}`.\n- At the start of execution, call `{transition_tool}` to move the issue to `{in_progress}` and add a brief `{comment_tool}` comment that you started work on run `{run_id}` attempt `{attempt}`.\n- When the implementation is ready, commit the lane, push branch `{branch}`, and create or update a non-draft PR for that branch.\n- After the PR is ready, call `{review_handoff_tool}` with the PR URL and a short result summary.\n- Do not move the issue directly to `{success}` with `{transition_tool}`. `maestro` will complete the success writeback only after its own validation passes.\n- If you determine the issue needs human attention, add label `{needs_attention}` with `{label_tool}` and explain the exact observed blocker in a comment, including the failed command and raw error when available. Do not speculate about capabilities you did not directly verify. Do not call `{review_handoff_tool}` in that case; `maestro` will stop the lane as a human-required failure without automatic retry.\n- Never write to any other issue.",
		issue = issue_run.issue.identifier,
		transition_tool = ISSUE_TRANSITION_TOOL_NAME,
		comment_tool = ISSUE_COMMENT_TOOL_NAME,
		label_tool = ISSUE_LABEL_ADD_TOOL_NAME,
		review_handoff_tool = ISSUE_REVIEW_HANDOFF_TOOL_NAME,
		in_progress = workflow.frontmatter().tracker().in_progress_state(),
		run_id = issue_run.run_id,
		attempt = issue_run.attempt_number,
		branch = issue_run.workspace.branch_name,
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
		"Resolve Linear issue {identifier}: {title}\n\nDescription:\n{description}\n\nExecution checklist:\n- Move the issue to `{in_progress}` with `{transition_tool}` and leave a short `{comment_tool}` comment that includes run `{run_id}` attempt `{attempt}`.\n- Keep discovery bounded to the minimal implementation files needed for this issue; defer broader docs or upstream reading unless a concrete ambiguity blocks the change.\n- Implement the fix in the current workspace.\n- Run the repository validation needed to justify a reviewable PR.\n- Commit the lane, push branch `{branch}`, and create or update a non-draft PR for that branch.\n- Call `{review_handoff_tool}` with the PR URL and a short result summary. Do not move the issue directly to `{success}` with `{transition_tool}`; `maestro` will finish that writeback after its own validation passes.\n- If the issue needs manual attention, add label `{needs_attention}` with `{label_tool}` and explain why in a comment. Do not call `{review_handoff_tool}` in that case; `maestro` will stop the lane as a human-required failure without automatic retry.",
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
		review_handoff_tool = ISSUE_REVIEW_HANDOFF_TOOL_NAME,
		in_progress = workflow.frontmatter().tracker().in_progress_state(),
		run_id = issue_run.run_id,
		attempt = issue_run.attempt_number,
		branch = issue_run.workspace.branch_name,
		success = workflow.frontmatter().tracker().success_state(),
		needs_attention = workflow.frontmatter().tracker().needs_attention_label(),
	)
}

fn run_validation_commands(commands: &[String], cwd: &Path) -> crate::prelude::Result<()> {
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

fn relative_workspace_path(project: &ServiceConfig, workspace: &WorkspaceSpec) -> String {
	relative_workspace_path_for_path(project, &workspace.path)
}

fn relative_workspace_path_for_path(project: &ServiceConfig, workspace_path: &Path) -> String {
	if let Ok(relative_path) = workspace_path.strip_prefix(project.repo_root()) {
		return relative_path.display().to_string();
	}
	if let Some(root_name) = project.workspace_root().file_name()
		&& let Ok(relative_path) = workspace_path.strip_prefix(project.workspace_root())
	{
		return Path::new(root_name).join(relative_path).display().to_string();
	}

	workspace_path.file_name().map_or_else(
		|| workspace_path.display().to_string(),
		|path| path.to_string_lossy().into_owned(),
	)
}

fn build_operator_status_snapshot(
	project: &ServiceConfig,
	state_store: &StateStore,
	limit: usize,
) -> crate::prelude::Result<OperatorStatusSnapshot> {
	let active_runs = state_store
		.list_active_runs(project.id())?
		.into_iter()
		.map(|run| operator_run_status(project, run))
		.collect::<Vec<_>>();
	let recent_runs = state_store
		.list_recent_runs(project.id(), limit)?
		.into_iter()
		.map(|run| operator_run_status(project, run))
		.collect::<Vec<_>>();
	let workspaces = state_store
		.list_workspaces(project.id())?
		.into_iter()
		.map(|mapping| OperatorWorkspaceStatus {
			issue_id: mapping.issue_id().to_owned(),
			branch_name: mapping.branch_name().to_owned(),
			workspace_path: relative_workspace_path_for_path(project, mapping.workspace_path()),
		})
		.collect::<Vec<_>>();

	Ok(OperatorStatusSnapshot {
		project_id: project.id().to_owned(),
		run_limit: limit,
		active_runs,
		recent_runs,
		workspaces,
	})
}

fn operator_run_status(project: &ServiceConfig, run: ProjectRunStatus) -> OperatorRunStatus {
	OperatorRunStatus {
		run_id: run.run_id().to_owned(),
		issue_id: run.issue_id().to_owned(),
		attempt_number: run.attempt_number(),
		status: run.status().to_owned(),
		thread_id: run.thread_id().map(str::to_owned),
		active_lease: run.active_lease(),
		updated_at: run.updated_at().to_owned(),
		last_event_type: run.last_event_type().map(str::to_owned),
		last_event_at: run.last_event_at().map(str::to_owned),
		event_count: run.event_count(),
		branch_name: run.branch_name().map(str::to_owned),
		workspace_path: run
			.workspace_path()
			.map(|path| relative_workspace_path_for_path(project, path)),
	}
}

fn render_operator_status(snapshot: &OperatorStatusSnapshot) -> String {
	let mut output = String::new();

	output.push_str(&format!("Project: {}\n", snapshot.project_id));
	output.push_str(&format!("Active runs: {}\n", snapshot.active_runs.len()));
	output.push_str(&format!("Recent runs shown: {}\n", snapshot.recent_runs.len()));
	output.push_str(&format!("Retained workspaces: {}\n", snapshot.workspaces.len()));
	output.push_str("\nActive Runs\n");

	if snapshot.active_runs.is_empty() {
		output.push_str("- none\n");
	} else {
		for run in &snapshot.active_runs {
			append_rendered_run(&mut output, run);
		}
	}

	output.push_str("\nRecent Runs\n");

	if snapshot.recent_runs.is_empty() {
		output.push_str("- none\n");
	} else {
		for run in &snapshot.recent_runs {
			append_rendered_run(&mut output, run);
		}
	}

	output.push_str("\nRetained Workspaces\n");

	if snapshot.workspaces.is_empty() {
		output.push_str("- none\n");
	} else {
		for workspace in &snapshot.workspaces {
			output.push_str(&format!(
				"- issue_id: {}\n  branch: {}\n  workspace_path: {}\n",
				workspace.issue_id, workspace.branch_name, workspace.workspace_path
			));
		}
	}

	output
}

fn append_rendered_run(output: &mut String, run: &OperatorRunStatus) {
	let last_event = match (&run.last_event_type, &run.last_event_at) {
		(Some(event_type), Some(timestamp)) => format!("{event_type} @ {timestamp}"),
		(Some(event_type), None) => event_type.clone(),
		(None, Some(timestamp)) => timestamp.clone(),
		(None, None) => String::from("none"),
	};
	let thread_id = run.thread_id.as_deref().unwrap_or("none");
	let branch_name = run.branch_name.as_deref().unwrap_or("none");
	let workspace_path = run.workspace_path.as_deref().unwrap_or("none");

	output.push_str(&format!(
		"- run_id: {}\n  issue_id: {}\n  attempt: {}\n  status: {}\n  active_lease: {}\n  thread_id: {}\n  branch: {}\n  workspace_path: {}\n  updated_at: {}\n  last_event: {}\n  event_count: {}\n",
		run.run_id,
		run.issue_id,
		run.attempt_number,
		run.status,
		if run.active_lease { "yes" } else { "no" },
		thread_id,
		branch_name,
		workspace_path,
		run.updated_at,
		last_event,
		run.event_count
	));
}

fn select_issue_candidate(
	issues: Vec<TrackerIssue>,
	workflow: &WorkflowDocument,
	state_store: &StateStore,
	project_id: &str,
) -> crate::prelude::Result<Option<TrackerIssue>> {
	if !has_available_dispatch_slot(project_id, state_store)? {
		return Ok(None);
	}

	let mut eligible_issues = Vec::new();

	for issue in issues {
		if is_issue_eligible(&issue, workflow, state_store)? {
			eligible_issues.push(issue);
		}
	}

	eligible_issues.sort_by(compare_issue_candidates);

	Ok(eligible_issues.into_iter().next())
}

fn compare_issue_candidates(left: &TrackerIssue, right: &TrackerIssue) -> Ordering {
	let left_priority = (left.priority.is_none(), left.priority.unwrap_or(i64::MAX));
	let right_priority = (right.priority.is_none(), right.priority.unwrap_or(i64::MAX));

	left_priority
		.cmp(&right_priority)
		.then_with(|| left.created_at.cmp(&right.created_at))
		.then_with(|| left.identifier.cmp(&right.identifier))
}

fn format_retry_comment(
	run_id: &str,
	attempt_number: i64,
	max_attempts: i64,
	workspace_path: String,
	branch_name: &str,
	error: &Report,
) -> String {
	format!(
		"maestro run failed and will retry\n\n- run_id: `{run_id}`\n- attempt: `{attempt_number}` / `{max_attempts}`\n- failed_at: `{failed_at}`\n- branch: `{branch}`\n- workspace_path: `{workspace}`\n- error_class: `retryable_execution_failure`\n- next_action: `maestro will retry automatically`\n- error: `{error}`",
		failed_at = current_timestamp(),
		branch = branch_name,
		workspace = workspace_path
	)
}

fn format_terminal_failure_comment(
	run_id: &str,
	attempt_number: i64,
	workspace_path: String,
	branch_name: &str,
	recovery_gate: &str,
	manual_attention_requested: bool,
	error: &Report,
) -> String {
	let (error_class, next_action) = if manual_attention_requested {
		(
			"human_attention_required",
			format!(
				"inspect the issue comment and workspace, resolve the blocker manually, {recovery_gate}"
			),
		)
	} else if error.downcast_ref::<ReviewHandoffNeedsAttention>().is_some() {
		(
			"review_handoff_writeback_failed",
			format!(
				"inspect the tracker state, PR, and workspace, repair the incomplete review handoff manually, {recovery_gate}"
			),
		)
	} else if error.downcast_ref::<StalledRunNeedsAttention>().is_some() {
		(
			"stalled_run_detected",
			format!(
				"inspect the workspace and app-server activity for the stalled lane, resolve the blocker manually, {recovery_gate}"
			),
		)
	} else {
		(
			"retry_budget_exhausted",
			format!("inspect the workspace, resolve the issue manually, {recovery_gate}"),
		)
	};

	format!(
		"maestro run failed and needs attention\n\n- run_id: `{run_id}`\n- attempt: `{attempt_number}`\n- failed_at: `{failed_at}`\n- branch: `{branch}`\n- workspace_path: `{workspace}`\n- error_class: `{error_class}`\n- next_action: `{next_action}`\n- error: `{error}`",
		failed_at = current_timestamp(),
		branch = branch_name,
		workspace = workspace_path
	)
}

fn terminal_failure_recovery_gate(
	needs_attention_label: &str,
	needs_attention_label_available: bool,
	guarded_by_nonstartable_state: bool,
	nonstartable_guard_state: &str,
) -> String {
	if needs_attention_label_available {
		return format!(
			"clear label `{needs_attention_label}`, then move the issue back to a startable state if another automated run is desired"
		);
	}
	if guarded_by_nonstartable_state {
		return format!(
			"`{needs_attention_label}` could not be applied because it does not exist on the team; the issue remains in `{nonstartable_guard_state}` to block automatic retries, so move it back to a startable state manually if another automated run is desired"
		);
	}

	format!(
		"`{needs_attention_label}` could not be applied because it does not exist on the team; move the issue back to a startable state manually if another automated run is desired"
	)
}

fn current_timestamp() -> String {
	OffsetDateTime::now_utc().format(&Rfc3339).expect("timestamp formatting should succeed")
}

fn build_run_id(issue_identifier: &str, attempt_number: i64) -> crate::prelude::Result<String> {
	let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

	Ok(format!("{}-attempt-{attempt_number}-{timestamp}", issue_identifier.to_lowercase()))
}

fn resolve_config_path(explicit_path: Option<&Path>) -> crate::prelude::Result<Option<PathBuf>> {
	if let Some(path) = explicit_path {
		return Ok(Some(path.to_path_buf()));
	}

	let repo_local = repo_local_config_path();

	if repo_local.exists() {
		return Ok(Some(repo_local));
	}

	let default_path = config::default_config_path()?;

	if default_path.exists() {
		return Ok(Some(default_path));
	}

	Ok(None)
}

fn repo_local_config_path() -> PathBuf {
	PathBuf::from("tmp/maestro.toml")
}

fn default_state_store_path() -> crate::prelude::Result<PathBuf> {
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
	use std::{
		cell::RefCell,
		fs,
		path::{Path, PathBuf},
		process::Command,
		time::Duration,
	};

	use color_eyre::Report;
	use tempfile::TempDir;
	use time::OffsetDateTime;

	use crate::{
		agent::ACTIVE_RUN_IDLE_TIMEOUT,
		config::ServiceConfig,
		orchestrator::{
			self, ISSUE_COMMENT_TOOL_NAME, ISSUE_LABEL_ADD_TOOL_NAME,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME, ISSUE_TRANSITION_TOOL_NAME, RunSummary,
		},
		prelude::Result,
		state::StateStore,
		tracker::{
			IssueTracker, TrackerIssue, TrackerIssueBlocker, TrackerLabel, TrackerProject,
			TrackerState, TrackerTeam,
		},
		workflow::WorkflowDocument,
		workspace::WorkspaceSpec,
	};

	struct FakeTracker {
		listed_issues: Vec<TrackerIssue>,
		project_exists: bool,
		refresh_snapshots: RefCell<Vec<Vec<TrackerIssue>>>,
		refresh_error: RefCell<Option<String>>,
		comments: RefCell<Vec<String>>,
		state_updates: RefCell<Vec<(String, String)>>,
		label_updates: RefCell<Vec<(String, Vec<String>)>>,
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
				refresh_error: RefCell::new(None),
				comments: RefCell::new(Vec::new()),
				state_updates: RefCell::new(Vec::new()),
				label_updates: RefCell::new(Vec::new()),
			}
		}

		fn with_refresh_error(listed_issues: Vec<TrackerIssue>, message: &str) -> Self {
			let tracker = Self::with_refresh_snapshots_and_project(
				listed_issues.clone(),
				vec![listed_issues],
				true,
			);

			*tracker.refresh_error.borrow_mut() = Some(message.to_owned());

			tracker
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
			if let Some(message) = self.refresh_error.borrow_mut().take() {
				return Err(Report::msg(message));
			}

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
			self.state_updates.borrow_mut().push((_issue_id.to_owned(), _state_id.to_owned()));

			Ok(())
		}

		fn update_issue_labels(&self, _issue_id: &str, _label_ids: &[String]) -> Result<()> {
			self.label_updates.borrow_mut().push((_issue_id.to_owned(), _label_ids.to_vec()));

			Ok(())
		}

		fn create_comment(&self, _issue_id: &str, body: &str) -> Result<()> {
			self.comments.borrow_mut().push(body.to_owned());

			Ok(())
		}
	}

	fn sample_issue(state_name: &str, labels: &[&str]) -> TrackerIssue {
		sample_issue_with_sort_fields(
			"issue-1",
			"PUB-101",
			state_name,
			labels,
			Some(3),
			"2026-03-13T04:16:17.133Z",
		)
	}

	fn sample_blocker(id: &str, identifier: &str, state_name: &str) -> TrackerIssueBlocker {
		TrackerIssueBlocker {
			id: id.to_owned(),
			identifier: identifier.to_owned(),
			state: TrackerState { id: format!("state-{id}"), name: state_name.to_owned() },
		}
	}

	fn sample_issue_with_sort_fields(
		id: &str,
		identifier: &str,
		state_name: &str,
		labels: &[&str],
		priority: Option<i64>,
		created_at: &str,
	) -> TrackerIssue {
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
			id: id.to_owned(),
			identifier: identifier.to_owned(),
			title: String::from("Implement orchestration"),
			description: String::from("Body"),
			priority,
			created_at: created_at.to_owned(),
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
			blockers: Vec::new(),
		}
	}

	fn sample_issue_without_needs_attention_team_label(
		state_name: &str,
		labels: &[&str],
	) -> TrackerIssue {
		let mut issue = sample_issue(state_name, labels);

		issue.team.labels.retain(|label| label.name != "maestro:needs-attention");

		issue
	}

	fn temp_project_layout() -> (TempDir, ServiceConfig, WorkflowDocument) {
		temp_project_layout_with_read_first(
			&[("AGENTS.md", "Read me first.\n")],
			"Follow the repository policy.\n",
		)
	}

	fn temp_project_layout_with_read_first(
		read_first_files: &[(&str, &str)],
		workflow_body: &str,
	) -> (TempDir, ServiceConfig, WorkflowDocument) {
		let temp_dir = TempDir::new().expect("temp dir should exist");
		let repo_root = temp_dir.path().join("target-repo");
		let workspace_root = temp_dir.path().join("workspaces");
		let read_first_paths = read_first_files.iter().map(|(path, _)| *path).collect::<Vec<_>>();

		fs::create_dir_all(&repo_root).expect("repo root should exist");
		fs::create_dir_all(&workspace_root).expect("workspace root should exist");

		for (relative_path, contents) in read_first_files {
			let absolute_path = repo_root.join(relative_path);

			if let Some(parent) = absolute_path.parent() {
				fs::create_dir_all(parent).expect("read_first parent should exist");
			}

			fs::write(absolute_path, contents).expect("read_first file should exist");
		}

		fs::write(
			repo_root.join("WORKFLOW.md"),
			sample_workflow_markdown(&read_first_paths, workflow_body),
		)
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
				.args(["config", "commit.gpgsign", "false"])
				.current_dir(&repo_root)
				.status()
				.expect("git config should run")
				.success()
		);
		assert!(
			Command::new("git")
				.args(["add", "."])
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

	fn sample_workflow_markdown(read_first: &[&str], workflow_body: &str) -> String {
		let read_first =
			read_first.iter().map(|path| format!("\"{path}\"")).collect::<Vec<_>>().join(", ");

		format!(
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
read_first = [{read_first}]
+++

{workflow_body}"#
		)
	}

	fn expected_developer_instructions(
		read_first_files: &[(&str, &str)],
		workflow: &WorkflowDocument,
		issue_run: &orchestrator::IssueRunPlan,
	) -> String {
		let mut sections = read_first_files
			.iter()
			.map(|(relative_path, contents)| format!("File: {relative_path}\n{contents}"))
			.collect::<Vec<_>>();

		sections.push(String::from(
			"Execution discipline\n- Keep pre-edit discovery bounded to the smallest code surface that can satisfy the current issue.\n- Start with the implementation files directly implicated by the issue before reading broader docs or repo-wide guidance.\n- Do not browse upstream references or general repository documentation unless a concrete ambiguity blocks the change.\n- Once the relevant change surface is identified, patch code and run validation instead of continuing broad searches.",
		));

		sections.push(format!(
			"Tracker tool contract\n- You own issue-scoped tracker writes for `{issue}`.\n- At the start of execution, call `{transition_tool}` to move the issue to `{in_progress}` and add a brief `{comment_tool}` comment that you started work on run `{run_id}` attempt `{attempt}`.\n- When the implementation is ready, commit the lane, push branch `{branch}`, and create or update a non-draft PR for that branch.\n- After the PR is ready, call `{review_handoff_tool}` with the PR URL and a short result summary.\n- Do not move the issue directly to `{success}` with `{transition_tool}`. `maestro` will complete the success writeback only after its own validation passes.\n- If you determine the issue needs human attention, add label `{needs_attention}` with `{label_tool}` and explain the exact observed blocker in a comment, including the failed command and raw error when available. Do not speculate about capabilities you did not directly verify. Do not call `{review_handoff_tool}` in that case; `maestro` will stop the lane as a human-required failure without automatic retry.\n- Never write to any other issue.",
			issue = issue_run.issue.identifier,
			transition_tool = ISSUE_TRANSITION_TOOL_NAME,
			comment_tool = ISSUE_COMMENT_TOOL_NAME,
			label_tool = ISSUE_LABEL_ADD_TOOL_NAME,
			review_handoff_tool = ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			in_progress = workflow.frontmatter().tracker().in_progress_state(),
			run_id = issue_run.run_id,
			attempt = issue_run.attempt_number,
			branch = issue_run.workspace.branch_name,
			success = workflow.frontmatter().tracker().success_state(),
			needs_attention = workflow.frontmatter().tracker().needs_attention_label(),
		));

		sections.join("\n\n")
	}

	#[test]
	fn eligibility_uses_state_label_blocker_and_lease_rules() {
		let (_, _, workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let eligible_issue = sample_issue("Todo", &[]);
		let opted_out_issue = sample_issue("Todo", &["maestro:manual-only"]);
		let needs_attention_issue = sample_issue("Todo", &["maestro:needs-attention"]);
		let mut blocked_issue = sample_issue("Todo", &[]);

		blocked_issue.blockers = vec![sample_blocker("issue-2", "PUB-102", "In Progress")];

		let mut unblocked_issue = sample_issue("Todo", &[]);

		unblocked_issue.blockers = vec![sample_blocker("issue-3", "PUB-103", "Done")];

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
			!orchestrator::is_issue_eligible(&needs_attention_issue, &workflow, &state_store)
				.expect("eligibility should succeed")
		);
		assert!(
			!orchestrator::is_issue_eligible(&blocked_issue, &workflow, &state_store)
				.expect("eligibility should succeed")
		);
		assert!(
			orchestrator::is_issue_eligible(&unblocked_issue, &workflow, &state_store)
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
				issue_id: String::from("issue-1"),
				issue_identifier: String::from("PUB-101"),
				branch_name: String::from("x/pubfi-pub-101"),
				workspace_path: Path::new(&config.workspace_root().join("PUB-101")).to_path_buf(),
				attempt_number: 1,
				run_id: summary.run_id.clone(),
			}
		);
		assert!(tracker.comments.borrow().is_empty());
	}

	#[test]
	fn developer_instructions_trim_workflow_body_and_preserve_required_guidance() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let issue = sample_issue("Todo", &[]);
		let issue_run = orchestrator::IssueRunPlan {
			issue,
			workspace: WorkspaceSpec {
				branch_name: String::from("x/pubfi-pub-101"),
				issue_identifier: String::from("PUB-101"),
				path: config.workspace_root().join("PUB-101"),
				reused_existing: false,
			},
			attempt_number: 1,
			run_id: String::from("pub-101-attempt-1-123"),
		};
		let instructions =
			orchestrator::build_developer_instructions(&config, &workflow, &issue_run)
				.expect("developer instructions should build");

		assert!(instructions.contains("File: AGENTS.md\nRead me first.\n"));
		assert!(instructions.contains("Keep pre-edit discovery bounded"));
		assert!(instructions.contains("Do not browse upstream references"));
		assert!(instructions.contains("Tracker tool contract"));
		assert!(instructions.contains("You own issue-scoped tracker writes for `PUB-101`."));
		assert!(
			instructions
				.contains("Do not speculate about capabilities you did not directly verify.")
		);
		assert!(instructions.contains(ISSUE_REVIEW_HANDOFF_TOOL_NAME));
		assert!(!instructions.contains("WORKFLOW.md\n"));
		assert!(!instructions.contains("Follow the repository policy."));
	}

	#[test]
	fn developer_instructions_match_trimmed_prompt_shape() {
		let read_first_files = [
			("AGENTS.md", "Read me first.\n"),
			("docs/index.md", "Use the documentation index.\n"),
		];
		let (_temp_dir, config, workflow) = temp_project_layout_with_read_first(
			&read_first_files,
			"This workflow body should never be appended.\n",
		);
		let issue = sample_issue("Todo", &[]);
		let issue_run = orchestrator::IssueRunPlan {
			issue,
			workspace: WorkspaceSpec {
				branch_name: String::from("x/pubfi-pub-101"),
				issue_identifier: String::from("PUB-101"),
				path: config.workspace_root().join("PUB-101"),
				reused_existing: false,
			},
			attempt_number: 1,
			run_id: String::from("pub-101-attempt-1-123"),
		};
		let instructions =
			orchestrator::build_developer_instructions(&config, &workflow, &issue_run)
				.expect("developer instructions should build");

		assert_eq!(
			instructions,
			expected_developer_instructions(&read_first_files, &workflow, &issue_run)
		);
	}

	#[test]
	fn candidate_selection_sorts_by_priority_created_at_and_identifier() {
		let (_temp_dir, _config, workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let high_priority = sample_issue_with_sort_fields(
			"issue-2",
			"PUB-102",
			"Todo",
			&[],
			Some(1),
			"2026-03-13T04:18:17.133Z",
		);
		let oldest_same_priority = sample_issue_with_sort_fields(
			"issue-3",
			"PUB-103",
			"Todo",
			&[],
			Some(2),
			"2026-03-13T04:15:17.133Z",
		);
		let newest_same_priority = sample_issue_with_sort_fields(
			"issue-4",
			"PUB-104",
			"Todo",
			&[],
			Some(2),
			"2026-03-13T04:19:17.133Z",
		);
		let no_priority = sample_issue_with_sort_fields(
			"issue-5",
			"PUB-105",
			"Todo",
			&[],
			None,
			"2026-03-13T04:14:17.133Z",
		);
		let selected = orchestrator::select_issue_candidate(
			vec![no_priority, newest_same_priority, oldest_same_priority, high_priority],
			&workflow,
			&state_store,
			"pubfi",
		)
		.expect("candidate selection should succeed")
		.expect("one issue should be selected");

		assert_eq!(selected.identifier, "PUB-102");
	}

	#[test]
	fn candidate_selection_breaks_ties_by_identifier_after_created_at() {
		let (_temp_dir, _config, workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let later_identifier = sample_issue_with_sort_fields(
			"issue-2",
			"PUB-102",
			"Todo",
			&[],
			Some(2),
			"2026-03-13T04:16:17.133Z",
		);
		let earlier_identifier = sample_issue_with_sort_fields(
			"issue-3",
			"PUB-101",
			"Todo",
			&[],
			Some(2),
			"2026-03-13T04:16:17.133Z",
		);
		let selected = orchestrator::select_issue_candidate(
			vec![later_identifier, earlier_identifier],
			&workflow,
			&state_store,
			"pubfi",
		)
		.expect("candidate selection should succeed")
		.expect("one issue should be selected");

		assert_eq!(selected.identifier, "PUB-101");
	}

	#[test]
	fn candidate_selection_skips_todo_issue_with_nonterminal_blockers() {
		let (_temp_dir, _config, workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let mut blocked_high_priority = sample_issue_with_sort_fields(
			"issue-2",
			"PUB-102",
			"Todo",
			&[],
			Some(1),
			"2026-03-13T04:15:17.133Z",
		);

		blocked_high_priority.blockers = vec![sample_blocker("issue-9", "PUB-109", "In Progress")];

		let unblocked_lower_priority = sample_issue_with_sort_fields(
			"issue-3",
			"PUB-103",
			"Todo",
			&[],
			Some(2),
			"2026-03-13T04:16:17.133Z",
		);
		let selected = orchestrator::select_issue_candidate(
			vec![blocked_high_priority, unblocked_lower_priority],
			&workflow,
			&state_store,
			"pubfi",
		)
		.expect("candidate selection should succeed")
		.expect("one issue should be selected");

		assert_eq!(selected.identifier, "PUB-103");
	}

	#[test]
	fn candidate_selection_respects_single_dispatch_slot() {
		let (_temp_dir, _config, workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");

		state_store.upsert_lease("pubfi", "issue-active", "run-1").expect("lease should record");

		let selected = orchestrator::select_issue_candidate(
			vec![sample_issue("Todo", &[])],
			&workflow,
			&state_store,
			"pubfi",
		)
		.expect("candidate selection should succeed");

		assert!(selected.is_none(), "project-level dispatch slot should block new selection");
	}

	#[test]
	fn failure_comments_use_repo_relative_workspace_paths() {
		let (_temp_dir, config, _workflow) = temp_project_layout();
		let workspace = WorkspaceSpec {
			branch_name: String::from("x/pubfi-pub-101"),
			issue_identifier: String::from("PUB-101"),
			path: config.repo_root().join(".workspaces/PUB-101"),
			reused_existing: true,
		};

		assert_eq!(
			orchestrator::relative_workspace_path(&config, &workspace),
			".workspaces/PUB-101"
		);
	}

	#[test]
	fn repo_local_config_path_points_to_tmp_maestro_toml() {
		assert_eq!(orchestrator::repo_local_config_path(), PathBuf::from("tmp/maestro.toml"));
	}

	#[test]
	fn operator_status_snapshot_includes_active_runs_and_repo_relative_paths() {
		let (_temp_dir, config, _workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let issue = sample_issue("Todo", &[]);
		let workspace_path = config.workspace_root().join("PUB-101");

		state_store
			.record_run_attempt("run-1", &issue.id, 1, "running")
			.expect("run attempt should record");
		state_store.update_run_thread("run-1", "thread-1").expect("thread id should attach");
		state_store.upsert_lease("pubfi", &issue.id, "run-1").expect("lease should record");
		state_store
			.upsert_workspace(
				"pubfi",
				&issue.id,
				"x/pubfi-pub-101",
				&workspace_path.display().to_string(),
			)
			.expect("workspace should record");
		state_store
			.append_event("run-1", 1, "turn/completed", "{\"turn\":\"1\"}")
			.expect("event should record");

		let snapshot = orchestrator::build_operator_status_snapshot(&config, &state_store, 10)
			.expect("snapshot should build");

		assert_eq!(snapshot.project_id, "pubfi");
		assert_eq!(snapshot.active_runs.len(), 1);
		assert_eq!(snapshot.recent_runs.len(), 1);
		assert_eq!(snapshot.active_runs[0].run_id, "run-1");
		assert_eq!(snapshot.active_runs[0].thread_id.as_deref(), Some("thread-1"));
		assert_eq!(snapshot.active_runs[0].branch_name.as_deref(), Some("x/pubfi-pub-101"));
		assert_eq!(snapshot.active_runs[0].workspace_path.as_deref(), Some("workspaces/PUB-101"));
		assert_eq!(snapshot.active_runs[0].last_event_type.as_deref(), Some("turn/completed"));
		assert_eq!(snapshot.workspaces[0].workspace_path, "workspaces/PUB-101");
	}

	#[test]
	fn operator_status_snapshot_keeps_all_active_runs_when_recent_runs_are_limited() {
		let (_temp_dir, config, _workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let first_issue = sample_issue_with_sort_fields(
			"issue-1",
			"PUB-101",
			"Todo",
			&[],
			Some(3),
			"2026-03-13T04:16:17.133Z",
		);
		let second_issue = sample_issue_with_sort_fields(
			"issue-2",
			"PUB-102",
			"Todo",
			&[],
			Some(3),
			"2026-03-13T04:17:17.133Z",
		);

		for (run_id, issue, branch_suffix) in
			[("run-1", &first_issue, "101"), ("run-2", &second_issue, "102")]
		{
			state_store
				.record_run_attempt(run_id, &issue.id, 1, "running")
				.expect("run attempt should record");
			state_store.upsert_lease("pubfi", &issue.id, run_id).expect("lease should record");
			state_store
				.upsert_workspace(
					"pubfi",
					&issue.id,
					&format!("x/pubfi-pub-{branch_suffix}"),
					&config.workspace_root().join(&issue.identifier).display().to_string(),
				)
				.expect("workspace should record");
		}

		let snapshot = orchestrator::build_operator_status_snapshot(&config, &state_store, 1)
			.expect("snapshot should build");

		assert_eq!(snapshot.run_limit, 1);
		assert_eq!(snapshot.recent_runs.len(), 1);
		assert_eq!(snapshot.active_runs.len(), 2);
		assert!(snapshot.active_runs.iter().all(|run| run.active_lease));
	}

	#[test]
	fn operator_status_text_renders_human_readable_sections() {
		let snapshot = orchestrator::OperatorStatusSnapshot {
			project_id: String::from("pubfi"),
			run_limit: 10,
			active_runs: vec![orchestrator::OperatorRunStatus {
				run_id: String::from("run-1"),
				issue_id: String::from("issue-1"),
				attempt_number: 1,
				status: String::from("running"),
				thread_id: Some(String::from("thread-1")),
				active_lease: true,
				updated_at: String::from("2026-03-14 10:00:00"),
				last_event_type: Some(String::from("turn/completed")),
				last_event_at: Some(String::from("2026-03-14 10:00:01")),
				event_count: 4,
				branch_name: Some(String::from("x/pubfi-pub-101")),
				workspace_path: Some(String::from(".workspaces/PUB-101")),
			}],
			recent_runs: vec![],
			workspaces: vec![orchestrator::OperatorWorkspaceStatus {
				issue_id: String::from("issue-1"),
				branch_name: String::from("x/pubfi-pub-101"),
				workspace_path: String::from(".workspaces/PUB-101"),
			}],
		};
		let rendered = orchestrator::render_operator_status(&snapshot);

		assert!(rendered.contains("Project: pubfi"));
		assert!(rendered.contains("Active Runs"));
		assert!(rendered.contains("run_id: run-1"));
		assert!(rendered.contains("last_event: turn/completed @ 2026-03-14 10:00:01"));
		assert!(rendered.contains("Retained Workspaces"));
		assert!(rendered.contains("workspace_path: .workspaces/PUB-101"));
	}

	#[test]
	fn human_required_terminal_failure_comments_use_manual_attention_error_class() {
		let error = Report::new(super::ManualAttentionRequested {
			issue_identifier: String::from("PUB-101"),
			label: String::from("maestro:needs-attention"),
			run_id: String::from("pub-101-attempt-1-123"),
		});
		let comment = orchestrator::format_terminal_failure_comment(
			"pub-101-attempt-1-123",
			1,
			String::from(".workspaces/PUB-101"),
			"x/pubfi-pub-101",
			"clear label `maestro:needs-attention`, then move the issue back to a startable state if another automated run is desired",
			true,
			&error,
		);

		assert!(comment.contains("- error_class: `human_attention_required`"));
		assert!(comment.contains("stop automatic retries and hand off manually"));
		assert!(comment.contains("clear label `maestro:needs-attention`"));
	}

	#[test]
	fn review_handoff_writeback_failures_use_nonretryable_terminal_failure_comment() {
		let error = Report::new(super::ReviewHandoffNeedsAttention {
			issue_identifier: String::from("PUB-101"),
			run_id: String::from("pub-101-attempt-1-123"),
		});
		let comment = orchestrator::format_terminal_failure_comment(
			"pub-101-attempt-1-123",
			1,
			String::from(".workspaces/PUB-101"),
			"x/pubfi-pub-101",
			"clear label `maestro:needs-attention`, then move the issue back to a startable state if another automated run is desired",
			false,
			&error,
		);

		assert!(comment.contains("- error_class: `review_handoff_writeback_failed`"));
		assert!(comment.contains("repair the incomplete review handoff manually"));
		assert!(comment.contains("clear label `maestro:needs-attention`"));
	}

	#[test]
	fn terminal_failure_comments_explain_state_guard_when_needs_attention_label_is_unavailable() {
		let error = Report::new(super::StalledRunNeedsAttention {
			issue_identifier: String::from("PUB-101"),
			run_id: String::from("pub-101-attempt-1-123"),
			idle_for: ACTIVE_RUN_IDLE_TIMEOUT,
		});
		let comment = orchestrator::format_terminal_failure_comment(
			"pub-101-attempt-1-123",
			1,
			String::from(".workspaces/PUB-101"),
			"x/pubfi-pub-101",
			"`maestro:needs-attention` could not be applied because it does not exist on the team; the issue remains in `In Progress` to block automatic retries, so move it back to a startable state manually if another automated run is desired",
			false,
			&error,
		);

		assert!(comment.contains("- error_class: `stalled_run_detected`"));
		assert!(comment.contains("does not exist on the team"));
		assert!(comment.contains("remains in `In Progress`"));
	}

	#[test]
	fn live_runs_require_gh_preflight() {
		assert!(orchestrator::validate_review_handoff_runtime(true).is_ok());
		assert!(orchestrator::validate_command_available("git", "test preflight").is_ok());

		let error = orchestrator::validate_command_available(
			"__maestro_missing_command__",
			"PR-backed review handoff",
		)
		.expect_err("missing command should fail preflight");

		assert!(
			error
				.to_string()
				.contains("Required command `__maestro_missing_command__` is unavailable")
		);
	}

	#[test]
	fn reconciliation_clears_stale_leases_and_terminal_workspaces() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let issue = sample_issue("Done", &[]);
		let tracker = FakeTracker::new(vec![issue.clone()]);
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let workspace_path = config.workspace_root().join("PUB-101");

		state_store
			.record_run_attempt("run-1", &issue.id, 1, "running")
			.expect("run attempt should record");
		state_store.upsert_lease("pubfi", &issue.id, "run-1").expect("lease should record");
		state_store
			.upsert_workspace(
				"pubfi",
				&issue.id,
				"x/pubfi-pub-101",
				&workspace_path.display().to_string(),
			)
			.expect("workspace mapping should record");

		let summary =
			orchestrator::run_project_once(&tracker, &config, &workflow, &state_store, false)
				.expect("reconciliation should succeed");

		assert!(summary.is_none());
		assert!(
			state_store.lease_for_issue(&issue.id).expect("lease lookup should work").is_none()
		);
		assert!(
			state_store
				.workspace_for_issue(&issue.id)
				.expect("workspace lookup should work")
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
	fn exited_child_cleanup_clears_orphaned_active_run_state() {
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let issue = sample_issue("In Progress", &[]);

		state_store
			.record_run_attempt("run-1", &issue.id, 1, "running")
			.expect("run attempt should record");
		state_store.upsert_lease("pubfi", &issue.id, "run-1").expect("lease should record");

		orchestrator::clear_orphaned_daemon_child_state(&state_store, &issue.id, "run-1")
			.expect("orphaned child cleanup should succeed");

		assert!(
			state_store.lease_for_issue(&issue.id).expect("lease lookup should succeed").is_none()
		);
		assert_eq!(
			state_store
				.run_attempt("run-1")
				.expect("run attempt lookup should succeed")
				.expect("run attempt should exist")
				.status(),
			"interrupted"
		);
	}

	#[test]
	fn active_run_reconciliation_detects_terminal_nonactive_and_stalled_runs() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let terminal_issue = sample_issue_with_sort_fields(
			"issue-terminal",
			"PUB-201",
			"Done",
			&[],
			Some(3),
			"2026-03-13T04:16:17.133Z",
		);
		let nonactive_issue = sample_issue_with_sort_fields(
			"issue-nonactive",
			"PUB-202",
			"Blocked",
			&[],
			Some(3),
			"2026-03-13T04:16:17.133Z",
		);
		let stalled_issue = sample_issue_with_sort_fields(
			"issue-stalled",
			"PUB-203",
			"In Progress",
			&[],
			Some(3),
			"2026-03-13T04:16:17.133Z",
		);
		let tracker = FakeTracker::new(vec![
			terminal_issue.clone(),
			nonactive_issue.clone(),
			stalled_issue.clone(),
		]);

		for issue in [&terminal_issue, &nonactive_issue, &stalled_issue] {
			state_store
				.record_run_attempt(&format!("run-{}", issue.identifier), &issue.id, 1, "running")
				.expect("run attempt should record");
			state_store
				.upsert_lease("pubfi", &issue.id, &format!("run-{}", issue.identifier))
				.expect("lease should record");
		}

		let now = OffsetDateTime::now_utc().unix_timestamp()
			+ ACTIVE_RUN_IDLE_TIMEOUT.as_secs() as i64
			+ 1;
		let actions = orchestrator::inspect_active_run_reconciliation_at(
			&tracker,
			&config,
			&workflow,
			&state_store,
			now,
		)
		.expect("active-run inspection should succeed");

		assert!(actions.iter().any(|action| {
			action.issue.id == terminal_issue.id
				&& matches!(action.disposition, orchestrator::ActiveRunDisposition::Terminal)
		}));
		assert!(actions.iter().any(|action| {
			action.issue.id == nonactive_issue.id
				&& matches!(action.disposition, orchestrator::ActiveRunDisposition::NonActive)
		}));
		assert!(actions.iter().any(|action| {
			action.issue.id == stalled_issue.id
				&& matches!(
				action.disposition,
				orchestrator::ActiveRunDisposition::Stalled { idle_for }
					if idle_for >= ACTIVE_RUN_IDLE_TIMEOUT
				)
		}));
	}

	#[test]
	fn stalled_idle_duration_ignores_future_last_activity() {
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let issue = sample_issue("In Progress", &[]);
		let run_id = "run-future-activity";

		state_store
			.record_run_attempt(run_id, &issue.id, 1, "running")
			.expect("run attempt should record");

		let last_activity = state_store
			.last_run_activity_unix_epoch(run_id)
			.expect("last activity lookup should succeed")
			.expect("run activity should exist");

		assert_eq!(
			orchestrator::stalled_idle_duration(
				&state_store,
				&state_store
					.run_attempt(run_id)
					.expect("run lookup should succeed")
					.expect("run attempt should exist"),
				last_activity - 1
			)
			.expect("idle duration should evaluate"),
			None
		);
	}

	#[test]
	fn stalled_protocol_idle_duration_ignores_future_protocol_activity() {
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let run_id = "run-protocol-future-activity";

		state_store
			.append_event(run_id, 1, "thread/status/changed", "{\"status\":\"active\"}")
			.expect("protocol event should record");

		let last_activity = state_store
			.last_protocol_activity_unix_epoch(run_id)
			.expect("protocol activity lookup should succeed")
			.expect("protocol activity should exist");

		assert_eq!(
			orchestrator::stalled_protocol_idle_duration(&state_store, run_id, last_activity - 1)
				.expect("protocol idle duration should evaluate"),
			None
		);
	}

	#[test]
	fn active_run_reconciliation_ignores_startable_preclaim_states() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let issue = sample_issue_with_sort_fields(
			"issue-startable",
			"PUB-204",
			"Todo",
			&[],
			Some(3),
			"2026-03-13T04:16:17.133Z",
		);
		let tracker = FakeTracker::new(vec![issue.clone()]);

		state_store
			.record_run_attempt("run-startable", &issue.id, 1, "running")
			.expect("run attempt should record");
		state_store.upsert_lease("pubfi", &issue.id, "run-startable").expect("lease should record");

		let now = OffsetDateTime::now_utc().unix_timestamp() + 1;
		let actions = orchestrator::inspect_active_run_reconciliation_at(
			&tracker,
			&config,
			&workflow,
			&state_store,
			now,
		)
		.expect("active-run inspection should succeed");

		assert!(actions.is_empty(), "startable pre-claim states should not be interrupted");
	}

	#[test]
	fn active_run_reconciliation_keeps_nonterminal_nonactive_workspaces() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let tracker = FakeTracker::new(vec![]);
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let workspace_manager = crate::workspace::WorkspaceManager::new(
			"pubfi",
			config.repo_root(),
			config.workspace_root(),
		);
		let issue = sample_issue("Todo", &[]);
		let run_id = "run-nonactive";
		let workspace_path = config.workspace_root().join("PUB-101");

		state_store
			.record_run_attempt(run_id, &issue.id, 1, "running")
			.expect("run attempt should record");
		state_store.upsert_lease("pubfi", &issue.id, run_id).expect("lease should record");
		state_store
			.upsert_workspace(
				"pubfi",
				&issue.id,
				"x/pubfi-pub-101",
				&workspace_path.display().to_string(),
			)
			.expect("workspace mapping should record");

		let action = orchestrator::ActiveRunReconciliation {
			issue: issue.clone(),
			run_attempt: state_store
				.run_attempt(run_id)
				.expect("run attempt query should succeed")
				.expect("run attempt should exist"),
			workspace_mapping: state_store
				.workspace_for_issue(&issue.id)
				.expect("workspace query should succeed"),
			disposition: orchestrator::ActiveRunDisposition::NonActive,
		};

		orchestrator::apply_active_run_reconciliation(
			&tracker,
			&config,
			&workflow,
			&state_store,
			&workspace_manager,
			vec![action],
		)
		.expect("reconciliation should succeed");

		assert!(
			state_store.lease_for_issue(&issue.id).expect("lease lookup should succeed").is_none()
		);
		assert!(
			state_store
				.workspace_for_issue(&issue.id)
				.expect("workspace lookup should succeed")
				.is_some()
		);
		assert_eq!(
			state_store
				.run_attempt(run_id)
				.expect("run attempt lookup should succeed")
				.expect("run attempt should exist")
				.status(),
			"interrupted"
		);
	}

	#[test]
	fn stalled_run_reconciliation_routes_to_needs_attention_without_cleanup() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let tracker = FakeTracker::new(vec![]);
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let workspace_manager = crate::workspace::WorkspaceManager::new(
			"pubfi",
			config.repo_root(),
			config.workspace_root(),
		);
		let issue = sample_issue("In Progress", &[]);
		let run_id = "run-stalled";
		let workspace_path = config.workspace_root().join("PUB-101");

		state_store
			.record_run_attempt(run_id, &issue.id, 1, "running")
			.expect("run attempt should record");
		state_store.upsert_lease("pubfi", &issue.id, run_id).expect("lease should record");
		state_store
			.upsert_workspace(
				"pubfi",
				&issue.id,
				"x/pubfi-pub-101",
				&workspace_path.display().to_string(),
			)
			.expect("workspace mapping should record");

		let action = orchestrator::ActiveRunReconciliation {
			issue: issue.clone(),
			run_attempt: state_store
				.run_attempt(run_id)
				.expect("run attempt query should succeed")
				.expect("run attempt should exist"),
			workspace_mapping: state_store
				.workspace_for_issue(&issue.id)
				.expect("workspace query should succeed"),
			disposition: orchestrator::ActiveRunDisposition::Stalled {
				idle_for: ACTIVE_RUN_IDLE_TIMEOUT + Duration::from_secs(1),
			},
		};

		orchestrator::apply_active_run_reconciliation(
			&tracker,
			&config,
			&workflow,
			&state_store,
			&workspace_manager,
			vec![action],
		)
		.expect("reconciliation should succeed");

		assert!(
			state_store.lease_for_issue(&issue.id).expect("lease lookup should succeed").is_none()
		);
		assert!(
			state_store
				.workspace_for_issue(&issue.id)
				.expect("workspace lookup should succeed")
				.is_some()
		);
		assert_eq!(
			state_store
				.run_attempt(run_id)
				.expect("run attempt lookup should succeed")
				.expect("run attempt should exist")
				.status(),
			"stalled"
		);
		assert!(tracker.comments.borrow().iter().any(|comment| {
			comment.contains("stalled_run_detected")
				&& comment.contains("needs attention")
				&& comment.contains("clear label `maestro:needs-attention`")
		}));
	}

	#[test]
	fn terminal_failures_without_needs_attention_label_use_nonstartable_guard_state() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let tracker = FakeTracker::new(vec![]);
		let issue = sample_issue_without_needs_attention_team_label("Todo", &[]);
		let issue_run = orchestrator::IssueRunPlan {
			issue: issue.clone(),
			workspace: WorkspaceSpec {
				branch_name: String::from("x/pubfi-pub-101"),
				issue_identifier: issue.identifier.clone(),
				path: config.workspace_root().join("PUB-101"),
				reused_existing: false,
			},
			attempt_number: 1,
			run_id: String::from("pub-101-attempt-1-123"),
		};
		let error = Report::new(super::ManualAttentionRequested {
			issue_identifier: issue.identifier.clone(),
			label: String::from("maestro:needs-attention"),
			run_id: issue_run.run_id.clone(),
		});

		orchestrator::handle_failure(&tracker, &config, &workflow, &issue_run, &error)
			.expect("terminal failure handling should succeed");

		assert_eq!(
			tracker.state_updates.borrow().last(),
			Some(&(issue.id.clone(), String::from("state-progress")))
		);
		assert!(tracker.label_updates.borrow().is_empty());
		assert!(tracker.comments.borrow().iter().any(|comment| {
			comment.contains("does not exist on the team")
				&& comment.contains("remains in `In Progress`")
		}));
	}

	#[test]
	fn manual_attention_failure_overrides_succeeded_run_status() {
		let state_store = StateStore::open_in_memory().expect("state store should open");

		state_store
			.record_run_attempt("run-1", "issue-1", 1, "succeeded")
			.expect("run attempt should record");

		orchestrator::persist_issue_run_outcome(&state_store, "run-1", false)
			.expect("failed outcome should persist");

		assert_eq!(
			state_store
				.run_attempt("run-1")
				.expect("run attempt lookup should succeed")
				.expect("run attempt should exist")
				.status(),
			"failed"
		);
	}

	#[test]
	fn exited_child_reconciliation_detects_stalled_failed_runs_from_protocol_idle() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let issue = sample_issue_with_sort_fields(
			"issue-stalled-after-exit",
			"PUB-205",
			"In Progress",
			&[],
			Some(3),
			"2026-03-13T04:16:17.133Z",
		);
		let tracker = FakeTracker::new(vec![issue.clone()]);
		let run_id = "run-stalled-after-exit";

		state_store
			.record_run_attempt(run_id, &issue.id, 1, "running")
			.expect("run attempt should record");
		state_store
			.append_event(run_id, 1, "thread/status/changed", "{\"status\":\"active\"}")
			.expect("protocol event should record");
		state_store
			.update_run_status(run_id, "failed")
			.expect("run should exit as failed before daemon inspects it");

		let last_protocol_activity = state_store
			.last_protocol_activity_unix_epoch(run_id)
			.expect("protocol activity query should succeed")
			.expect("protocol activity should exist");
		let actions = orchestrator::inspect_exited_daemon_child_reconciliation_at(
			&tracker,
			&config,
			&workflow,
			&state_store,
			&issue.id,
			run_id,
			last_protocol_activity + ACTIVE_RUN_IDLE_TIMEOUT.as_secs() as i64 + 1,
		)
		.expect("exited child inspection should succeed");

		assert!(actions.iter().any(|action| {
			action.issue.id == issue.id
				&& matches!(
					action.disposition,
					orchestrator::ActiveRunDisposition::Stalled { idle_for }
						if idle_for >= ACTIVE_RUN_IDLE_TIMEOUT
				)
		}));
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
				.workspace_for_issue(&listed_issue.id)
				.expect("workspace lookup should work")
				.is_some()
		);
		assert!(tracker.comments.borrow().is_empty());
	}

	#[test]
	fn live_run_clears_claimed_lease_when_refresh_fails_after_workspace_prepare() {
		let (_temp_dir, config, workflow) = temp_project_layout();
		let listed_issue = sample_issue("Todo", &[]);
		let tracker = FakeTracker::with_refresh_error(
			vec![listed_issue.clone()],
			"transient refresh failure",
		);
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let error =
			orchestrator::run_project_once(&tracker, &config, &workflow, &state_store, false)
				.expect_err("run once should propagate refresh failure");

		assert!(
			error.to_string().contains("transient refresh failure"),
			"error should surface the refresh failure"
		);
		assert!(
			state_store
				.lease_for_issue(&listed_issue.id)
				.expect("lease lookup should work")
				.is_none()
		);
	}
}
