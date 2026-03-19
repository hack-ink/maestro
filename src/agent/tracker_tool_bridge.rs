use std::{
	cell::RefCell,
	error::Error,
	fmt::{Display, Formatter},
	path::{Component, PathBuf},
	process::Command,
};

use color_eyre::Report;
use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
	prelude::eyre,
	tracker::{IssueTracker, TrackerIssue},
	workflow::WorkflowDocument,
};

pub(crate) const ISSUE_TRANSITION_TOOL_NAME: &str = "issue_transition";
pub(crate) const ISSUE_COMMENT_TOOL_NAME: &str = "issue_comment";
pub(crate) const ISSUE_LABEL_ADD_TOOL_NAME: &str = "issue_label_add";
pub(crate) const ISSUE_REVIEW_HANDOFF_TOOL_NAME: &str = "issue_review_handoff";
pub(crate) const ISSUE_TERMINAL_FINALIZE_TOOL_NAME: &str = "issue_terminal_finalize";

static GH_PULL_REQUEST_INSPECTOR: GhPullRequestInspector = GhPullRequestInspector;
static LOCAL_GIT_REPO_INSPECTOR: LocalGitRepoInspector = LocalGitRepoInspector;

pub(crate) trait DynamicToolHandler {
	fn tool_specs(&self) -> Vec<DynamicToolSpec>;
	fn handle_call(&self, tool_name: &str, arguments: Value) -> DynamicToolCallResponse;
	fn classify_turn_completion(
		&self,
		final_output: &str,
	) -> crate::prelude::Result<TurnCompletionStatus> {
		self.validate_turn_completion(final_output)?;

		Ok(TurnCompletionStatus::Complete)
	}
	fn validate_turn_completion(&self, _final_output: &str) -> crate::prelude::Result<()> {
		Ok(())
	}
}

pub(crate) trait PullRequestInspector {
	fn inspect_pull_request(
		&self,
		cwd: &std::path::Path,
		pr_url: &str,
	) -> std::result::Result<PullRequestDetails, String>;
}

pub(crate) trait LocalRepoInspector {
	fn inspect_local_repo(
		&self,
		cwd: &std::path::Path,
	) -> std::result::Result<LocalRepoDetails, String>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DynamicToolSpec {
	pub(crate) description: String,
	#[serde(rename = "inputSchema")]
	pub(crate) input_schema: Value,
	pub(crate) name: String,
}

pub(crate) struct TrackerToolBridge<'a> {
	tracker: &'a dyn IssueTracker,
	issue: &'a TrackerIssue,
	workflow: &'a WorkflowDocument,
	review_context: Option<ReviewHandoffContext>,
	pull_request_inspector: &'a dyn PullRequestInspector,
	local_repo_inspector: &'a dyn LocalRepoInspector,
	local_issue_state_name: RefCell<String>,
	local_opt_out_requested: RefCell<bool>,
	manual_attention_requested: RefCell<bool>,
	manual_attention_comment_recorded: RefCell<bool>,
	continuation_blocking_tracker_write: RefCell<Option<String>>,
	pending_review_handoff: RefCell<Option<PendingReviewHandoff>>,
	finalized_completion_path: RefCell<Option<RunCompletionDisposition>>,
}
impl<'a> TrackerToolBridge<'a> {
	#[cfg(test)]
	pub(crate) fn new(
		tracker: &'a dyn IssueTracker,
		issue: &'a TrackerIssue,
		workflow: &'a WorkflowDocument,
	) -> Self {
		Self {
			tracker,
			issue,
			workflow,
			review_context: None,
			pull_request_inspector: &GH_PULL_REQUEST_INSPECTOR,
			local_repo_inspector: &LOCAL_GIT_REPO_INSPECTOR,
			local_issue_state_name: RefCell::new(issue.state.name.clone()),
			local_opt_out_requested: RefCell::new(
				issue.has_label(workflow.frontmatter().tracker().opt_out_label()),
			),
			manual_attention_requested: RefCell::new(false),
			manual_attention_comment_recorded: RefCell::new(false),
			continuation_blocking_tracker_write: RefCell::new(None),
			pending_review_handoff: RefCell::new(None),
			finalized_completion_path: RefCell::new(None),
		}
	}

	fn with_review_handoff_inspectors(
		tracker: &'a dyn IssueTracker,
		issue: &'a TrackerIssue,
		workflow: &'a WorkflowDocument,
		review_context: ReviewHandoffContext,
		pull_request_inspector: &'a dyn PullRequestInspector,
		local_repo_inspector: &'a dyn LocalRepoInspector,
	) -> Self {
		Self {
			tracker,
			issue,
			workflow,
			review_context: Some(review_context),
			pull_request_inspector,
			local_repo_inspector,
			local_issue_state_name: RefCell::new(issue.state.name.clone()),
			local_opt_out_requested: RefCell::new(
				issue.has_label(workflow.frontmatter().tracker().opt_out_label()),
			),
			manual_attention_requested: RefCell::new(false),
			manual_attention_comment_recorded: RefCell::new(false),
			continuation_blocking_tracker_write: RefCell::new(None),
			pending_review_handoff: RefCell::new(None),
			finalized_completion_path: RefCell::new(None),
		}
	}

	pub(crate) fn with_review_handoff(
		tracker: &'a dyn IssueTracker,
		issue: &'a TrackerIssue,
		workflow: &'a WorkflowDocument,
		review_context: ReviewHandoffContext,
		pull_request_inspector: &'a dyn PullRequestInspector,
	) -> Self {
		Self::with_review_handoff_inspectors(
			tracker,
			issue,
			workflow,
			review_context,
			pull_request_inspector,
			&LOCAL_GIT_REPO_INSPECTOR,
		)
	}

	#[cfg(test)]
	pub(crate) fn with_review_handoff_for_test(
		tracker: &'a dyn IssueTracker,
		issue: &'a TrackerIssue,
		workflow: &'a WorkflowDocument,
		review_context: ReviewHandoffContext,
		pull_request_inspector: &'a dyn PullRequestInspector,
		local_repo_inspector: &'a dyn LocalRepoInspector,
	) -> Self {
		Self::with_review_handoff_inspectors(
			tracker,
			issue,
			workflow,
			review_context,
			pull_request_inspector,
			local_repo_inspector,
		)
	}

	pub(crate) fn with_run_context(
		tracker: &'a dyn IssueTracker,
		issue: &'a TrackerIssue,
		workflow: &'a WorkflowDocument,
		review_context: ReviewHandoffContext,
	) -> Self {
		Self::with_review_handoff(
			tracker,
			issue,
			workflow,
			review_context,
			&GH_PULL_REQUEST_INSPECTOR,
		)
	}

	fn build_tool_specs(&self) -> Vec<DynamicToolSpec> {
		let mut tool_specs = vec![
			DynamicToolSpec {
				name: ISSUE_TRANSITION_TOOL_NAME.to_owned(),
				description: String::from(
					"Move the currently leased issue to another allowed workflow state.",
				),
				input_schema: serde_json::json!({
					"type": "object",
					"properties": {
						"issue_id": { "type": "string" },
						"issue_identifier": { "type": "string" },
						"state": { "type": "string" }
					},
					"required": ["state"],
					"additionalProperties": false
				}),
			},
			DynamicToolSpec {
				name: ISSUE_COMMENT_TOOL_NAME.to_owned(),
				description: String::from("Add a comment to the currently leased issue."),
				input_schema: serde_json::json!({
					"type": "object",
					"properties": {
						"issue_id": { "type": "string" },
						"issue_identifier": { "type": "string" },
						"body": { "type": "string" }
					},
					"required": ["body"],
					"additionalProperties": false
				}),
			},
		];

		if self.review_context.is_some() {
			tool_specs.extend([
				DynamicToolSpec {
					name: ISSUE_REVIEW_HANDOFF_TOOL_NAME.to_owned(),
					description: String::from(
						"Record a PR-backed review handoff for the currently leased issue after the branch is pushed and a non-draft PR is ready for review.",
					),
					input_schema: serde_json::json!({
						"type": "object",
						"properties": {
							"issue_id": { "type": "string" },
							"issue_identifier": { "type": "string" },
							"pr_url": { "type": "string" },
							"summary": { "type": "string" }
						},
						"required": ["pr_url", "summary"],
						"additionalProperties": false
					}),
				},
				DynamicToolSpec {
					name: ISSUE_TERMINAL_FINALIZE_TOOL_NAME.to_owned(),
					description: String::from(
						"Finalize the current run's terminal tracker path after either PR-backed review handoff or the manual-attention exit has been fully recorded.",
					),
					input_schema: serde_json::json!({
						"type": "object",
						"properties": {
							"issue_id": { "type": "string" },
							"issue_identifier": { "type": "string" },
							"path": {
								"type": "string",
								"enum": ["review_handoff", "manual_attention"]
							}
						},
						"required": ["path"],
						"additionalProperties": false
					}),
				},
			]);
		}

		tool_specs.extend([DynamicToolSpec {
			name: ISSUE_LABEL_ADD_TOOL_NAME.to_owned(),
			description: String::from(
				"Add an allowed workflow label to the currently leased issue.",
			),
			input_schema: serde_json::json!({
				"type": "object",
				"properties": {
					"issue_id": { "type": "string" },
					"issue_identifier": { "type": "string" },
					"label": { "type": "string" }
				},
				"required": ["label"],
				"additionalProperties": false
			}),
		}]);

		tool_specs
	}

	fn handle_call_inner(&self, tool_name: &str, arguments: Value) -> DynamicToolCallResponse {
		match tool_name {
			ISSUE_TRANSITION_TOOL_NAME => self.handle_transition(arguments),
			ISSUE_COMMENT_TOOL_NAME => self.handle_comment(arguments),
			ISSUE_REVIEW_HANDOFF_TOOL_NAME => self.handle_review_handoff(arguments),
			ISSUE_LABEL_ADD_TOOL_NAME => self.handle_add_label(arguments),
			ISSUE_TERMINAL_FINALIZE_TOOL_NAME => self.handle_terminal_finalize(arguments),
			_ =>
				DynamicToolCallResponse::failure(format!("Unsupported tracker tool `{tool_name}`.")),
		}
	}

	fn handle_transition(&self, arguments: Value) -> DynamicToolCallResponse {
		let parsed = match serde_json::from_value::<TransitionArgs>(arguments) {
			Ok(parsed) => parsed,
			Err(error) => {
				return DynamicToolCallResponse::failure(format!(
					"Invalid `issue.transition` arguments: {error}"
				));
			},
		};

		if let Err(error) = self.ensure_issue_scope(&parsed.scope) {
			return DynamicToolCallResponse::failure(error);
		}

		let allowed_states = self.allowed_transition_states();

		if !allowed_states.iter().any(|state| state == &parsed.state) {
			let success_state = self.workflow.frontmatter().tracker().success_state();

			if parsed.state == success_state {
				return DynamicToolCallResponse::failure(format!(
					"State `{}` requires `{}` after the branch is pushed and a reviewable PR exists.",
					parsed.state, ISSUE_REVIEW_HANDOFF_TOOL_NAME
				));
			}

			return DynamicToolCallResponse::failure(format!(
				"State `{}` is outside the allowed tracker tool policy.",
				parsed.state
			));
		}

		let Some(state_id) = self.issue.state_id_for_name(&parsed.state) else {
			return DynamicToolCallResponse::failure(format!(
				"State `{}` does not exist on issue `{}`.",
				parsed.state, self.issue.identifier
			));
		};

		match self.tracker.update_issue_state(&self.issue.id, state_id) {
			Ok(()) => {
				self.local_issue_state_name.replace(parsed.state.clone());
				self.record_continuation_blocking_transition(&parsed.state);

				DynamicToolCallResponse::success(format!(
					"Issue `{}` moved to `{}`.",
					self.issue.identifier, parsed.state
				))
			},
			Err(error) => DynamicToolCallResponse::failure(format!(
				"Failed to move issue `{}` to `{}`: {error}",
				self.issue.identifier, parsed.state
			)),
		}
	}

	fn handle_comment(&self, arguments: Value) -> DynamicToolCallResponse {
		let parsed = match serde_json::from_value::<CommentArgs>(arguments) {
			Ok(parsed) => parsed,
			Err(error) => {
				return DynamicToolCallResponse::failure(format!(
					"Invalid `issue.comment` arguments: {error}"
				));
			},
		};

		if let Err(error) = self.ensure_issue_scope(&parsed.scope) {
			return DynamicToolCallResponse::failure(error);
		}

		if parsed.body.trim().is_empty() {
			return DynamicToolCallResponse::failure(String::from(
				"`issue.comment` requires a non-empty `body`.",
			));
		}

		if let Err(error) = validate_public_comment_body(&parsed.body) {
			return DynamicToolCallResponse::failure(error);
		}

		match self.tracker.create_comment(&self.issue.id, &parsed.body) {
			Ok(()) => {
				if *self.manual_attention_requested.borrow() {
					self.manual_attention_comment_recorded.replace(true);
				}

				DynamicToolCallResponse::success(format!(
					"Comment added to issue `{}`.",
					self.issue.identifier
				))
			},
			Err(error) => DynamicToolCallResponse::failure(format!(
				"Failed to add a comment to issue `{}`: {error}",
				self.issue.identifier
			)),
		}
	}

	fn handle_review_handoff(&self, arguments: Value) -> DynamicToolCallResponse {
		let parsed = match serde_json::from_value::<ReviewHandoffArgs>(arguments) {
			Ok(parsed) => parsed,
			Err(error) => {
				return DynamicToolCallResponse::failure(format!(
					"Invalid `issue.review_handoff` arguments: {error}"
				));
			},
		};

		if let Err(error) = self.ensure_issue_scope(&parsed.scope) {
			return DynamicToolCallResponse::failure(error);
		}

		let Some(review_context) = self.review_context.as_ref() else {
			return DynamicToolCallResponse::failure(String::from(
				"`issue_review_handoff` is unavailable for this run.",
			));
		};
		let pr_url = parsed.pr_url.trim();

		if pr_url.is_empty() {
			return DynamicToolCallResponse::failure(String::from(
				"`issue_review_handoff` requires a non-empty `pr_url`.",
			));
		}

		let summary = normalize_summary(&parsed.summary);

		if summary.is_empty() {
			return DynamicToolCallResponse::failure(String::from(
				"`issue_review_handoff` requires a non-empty `summary`.",
			));
		}

		let pull_request = match self.validate_review_handoff_pr(review_context, pr_url) {
			Ok(pull_request) => pull_request,
			Err(error) => return DynamicToolCallResponse::failure(error),
		};

		self.pending_review_handoff
			.borrow_mut()
			.replace(PendingReviewHandoff { pr_url: pull_request.url.clone(), summary });

		DynamicToolCallResponse::success(format!(
			"Recorded review handoff for issue `{}` with PR `{}`. Maestro will apply the completion comment and move the issue to `{}` after service validation passes.",
			self.issue.identifier,
			pull_request.url,
			self.workflow.frontmatter().tracker().success_state()
		))
	}

	fn handle_add_label(&self, arguments: Value) -> DynamicToolCallResponse {
		let parsed = match serde_json::from_value::<LabelArgs>(arguments) {
			Ok(parsed) => parsed,
			Err(error) => {
				return DynamicToolCallResponse::failure(format!(
					"Invalid `issue.label.add` arguments: {error}"
				));
			},
		};

		if let Err(error) = self.ensure_issue_scope(&parsed.scope) {
			return DynamicToolCallResponse::failure(error);
		}

		let allowed_labels = [
			self.workflow.frontmatter().tracker().opt_out_label(),
			self.workflow.frontmatter().tracker().needs_attention_label(),
		];

		if !allowed_labels.iter().any(|label| label == &parsed.label) {
			return DynamicToolCallResponse::failure(format!(
				"Label `{}` is outside the allowed tracker tool policy.",
				parsed.label
			));
		}

		let current_issue = match self.refreshed_issue_snapshot() {
			Ok(Some(issue)) => issue,
			Ok(None) => {
				return DynamicToolCallResponse::failure(format!(
					"Failed to refresh issue `{}` before updating labels: tracker returned no current snapshot.",
					self.issue.identifier
				));
			},
			Err(error) => {
				return DynamicToolCallResponse::failure(format!(
					"Failed to refresh issue `{}` before updating labels: {error}",
					self.issue.identifier
				));
			},
		};
		let manual_attention_label =
			parsed.label == self.workflow.frontmatter().tracker().needs_attention_label();
		let Some(label_id) = current_issue.label_id_for_name(&parsed.label) else {
			return DynamicToolCallResponse::failure(format!(
				"Label `{}` does not exist on issue `{}`.",
				parsed.label, self.issue.identifier
			));
		};
		let mut label_ids =
			current_issue.labels.iter().map(|label| label.id.clone()).collect::<Vec<_>>();

		if label_ids.iter().any(|existing| existing == label_id) {
			if manual_attention_label {
				self.manual_attention_requested.replace(true);
			} else if parsed.label == self.workflow.frontmatter().tracker().opt_out_label() {
				self.local_opt_out_requested.replace(true);
				self.record_continuation_blocking_write(format!(
					"`{ISSUE_LABEL_ADD_TOOL_NAME}` with label `{}`",
					parsed.label
				));
			}

			return DynamicToolCallResponse::success(format!(
				"Issue `{}` already has label `{}`.",
				self.issue.identifier, parsed.label
			));
		}

		label_ids.push(label_id.to_owned());

		match self.tracker.update_issue_labels(&self.issue.id, &label_ids) {
			Ok(()) => {
				if manual_attention_label {
					self.manual_attention_requested.replace(true);
				} else if parsed.label == self.workflow.frontmatter().tracker().opt_out_label() {
					self.local_opt_out_requested.replace(true);
					self.record_continuation_blocking_write(format!(
						"`{ISSUE_LABEL_ADD_TOOL_NAME}` with label `{}`",
						parsed.label
					));
				}

				DynamicToolCallResponse::success(format!(
					"Label `{}` added to issue `{}`.",
					parsed.label, self.issue.identifier
				))
			},
			Err(error) => DynamicToolCallResponse::failure(format!(
				"Failed to add label `{}` to issue `{}`: {error}",
				parsed.label, self.issue.identifier
			)),
		}
	}

	fn handle_terminal_finalize(&self, arguments: Value) -> DynamicToolCallResponse {
		let parsed = match serde_json::from_value::<TerminalFinalizeArgs>(arguments) {
			Ok(parsed) => parsed,
			Err(error) => {
				return DynamicToolCallResponse::failure(format!(
					"Invalid `issue.terminal_finalize` arguments: {error}"
				));
			},
		};

		if let Err(error) = self.ensure_issue_scope(&parsed.scope) {
			return DynamicToolCallResponse::failure(error);
		}

		let requested_path = match parsed.path.as_str() {
			"review_handoff" => RunCompletionDisposition::ReviewHandoff,
			"manual_attention" => RunCompletionDisposition::ManualAttention,
			other => {
				return DynamicToolCallResponse::failure(format!(
					"`{ISSUE_TERMINAL_FINALIZE_TOOL_NAME}` path must be `review_handoff` or `manual_attention`, not `{other}`."
				));
			},
		};
		let actual_path = match self.completion_disposition() {
			Ok(actual_path) => actual_path,
			Err(error) => return DynamicToolCallResponse::failure(error.to_string()),
		};

		if requested_path != actual_path {
			return DynamicToolCallResponse::failure(format!(
				"`{ISSUE_TERMINAL_FINALIZE_TOOL_NAME}` requested path `{}`, but the recorded terminal path is `{}`.",
				requested_path.as_str(),
				actual_path.as_str()
			));
		}

		self.finalized_completion_path.replace(Some(actual_path));

		DynamicToolCallResponse::success(format!(
			"Finalized terminal path `{}` for issue `{}`. You can only finish the turn after this succeeds.",
			actual_path.as_str(),
			self.issue.identifier
		))
	}

	fn ensure_issue_scope(&self, scope: &ScopeArgs) -> Result<(), String> {
		if let Some(issue_id) = scope.issue_id.as_deref()
			&& issue_id != self.issue.id
		{
			return Err(format!(
				"Tool call targeted issue id `{issue_id}`, but the leased issue id is `{}`.",
				self.issue.id
			));
		}
		if let Some(issue_identifier) = scope.issue_identifier.as_deref()
			&& issue_identifier != self.issue.identifier
		{
			return Err(format!(
				"Tool call targeted issue identifier `{issue_identifier}`, but the leased issue identifier is `{}`.",
				self.issue.identifier
			));
		}

		Ok(())
	}

	fn allowed_transition_states(&self) -> Vec<&str> {
		let tracker = self.workflow.frontmatter().tracker();
		let success_state = tracker.success_state();
		let mut states = tracker
			.startable_states()
			.iter()
			.map(String::as_str)
			.filter(|state| *state != success_state)
			.collect::<Vec<_>>();

		for state in [tracker.in_progress_state(), tracker.failure_state()] {
			if state != success_state && !states.iter().any(|existing| existing == &state) {
				states.push(state);
			}
		}

		states
	}

	fn refreshed_issue_snapshot(&self) -> crate::prelude::Result<Option<TrackerIssue>> {
		let issue_ids = [self.issue.id.clone()];
		let mut refreshed_issues = self.tracker.refresh_issues(&issue_ids)?;

		Ok(refreshed_issues.pop())
	}

	fn validate_review_handoff_pr(
		&self,
		review_context: &ReviewHandoffContext,
		pr_url: &str,
	) -> std::result::Result<PullRequestDetails, String> {
		let pull_request =
			self.pull_request_inspector.inspect_pull_request(&review_context.cwd, pr_url)?;
		let local_repo = self.local_repo_inspector.inspect_local_repo(&review_context.cwd)?;

		if pull_request.head_repository_owner != local_repo.repository_owner
			|| pull_request.head_repository_name != local_repo.repository_name
		{
			return Err(format!(
				"Pull request `{}` belongs to repository `{}/{}`, but the current lane repository is `{}/{}`.",
				pull_request.url,
				pull_request.head_repository_owner,
				pull_request.head_repository_name,
				local_repo.repository_owner,
				local_repo.repository_name
			));
		}
		if pull_request.head_ref_name != review_context.branch_name {
			return Err(format!(
				"Pull request `{}` is for branch `{}`, but the current lane branch is `{}`.",
				pull_request.url, pull_request.head_ref_name, review_context.branch_name
			));
		}
		if pull_request.head_ref_oid != local_repo.head_oid {
			return Err(format!(
				"Pull request `{}` points at commit `{}`, but the current lane HEAD is `{}`. Push the latest lane commit before review handoff.",
				pull_request.url, pull_request.head_ref_oid, local_repo.head_oid
			));
		}
		if pull_request.state != "OPEN" {
			return Err(format!(
				"Pull request `{}` is `{}`; it must be open for review handoff.",
				pull_request.url, pull_request.state
			));
		}
		if pull_request.is_draft {
			return Err(format!(
				"Pull request `{}` is still draft; mark it ready for review before handoff.",
				pull_request.url
			));
		}

		Ok(pull_request)
	}

	fn record_continuation_blocking_transition(&self, state: &str) {
		if state != self.workflow.frontmatter().tracker().in_progress_state() {
			self.record_continuation_blocking_write(format!(
				"`{ISSUE_TRANSITION_TOOL_NAME}` to state `{state}`"
			));
		}
	}

	fn record_continuation_blocking_write(&self, reason: String) {
		self.continuation_blocking_tracker_write.replace(Some(reason));
	}

	fn continuation_blocking_write_reason(&self) -> crate::prelude::Result<Option<String>> {
		let Some(reason) = self.continuation_blocking_tracker_write.borrow().clone() else {
			return Ok(None);
		};
		let issue = match self.refreshed_issue_snapshot()? {
			Some(issue) => issue,
			None => return Ok(Some(reason)),
		};
		let tracker_policy = self.workflow.frontmatter().tracker();
		let issue_still_active = issue.state.name == tracker_policy.in_progress_state()
			&& !issue.has_label(tracker_policy.opt_out_label())
			&& !issue.has_label(tracker_policy.needs_attention_label());

		if issue_still_active {
			return Ok(None);
		}

		Ok(Some(reason))
	}

	pub(crate) fn startup_transition_succeeded_locally(&self) -> bool {
		self.local_issue_state_name.borrow().as_str()
			== self.workflow.frontmatter().tracker().in_progress_state()
	}

	pub(crate) fn completion_disposition(
		&self,
	) -> crate::prelude::Result<RunCompletionDisposition> {
		let Some(review_context) = self.review_context.as_ref() else {
			eyre::bail!(
				"Review handoff context is unavailable for issue `{}`.",
				self.issue.identifier
			);
		};
		let manual_attention_requested = *self.manual_attention_requested.borrow();
		let manual_attention_comment_recorded = *self.manual_attention_comment_recorded.borrow();
		let review_handoff_recorded = self.pending_review_handoff.borrow().is_some();

		match (
			manual_attention_requested,
			manual_attention_comment_recorded,
			review_handoff_recorded,
		) {
			(false, false, true) => Ok(RunCompletionDisposition::ReviewHandoff),
			(true, true, false) => Ok(RunCompletionDisposition::ManualAttention),
			(true, false, false) => eyre::bail!(
				"Run `{}` requested human attention with label `{}`, but issue `{}` never recorded the required explanatory comment.",
				review_context.run_id,
				self.workflow.frontmatter().tracker().needs_attention_label(),
				self.issue.identifier
			),
			(true, _, true) => eyre::bail!(
				"Run `{}` recorded both `issue_review_handoff` and label `{}`. Use exactly one final handoff path.",
				review_context.run_id,
				self.workflow.frontmatter().tracker().needs_attention_label()
			),
			(false, false, false) => eyre::bail!(
				"Run `{}` completed, but issue `{}` recorded neither a PR-backed review handoff nor label `{}` for human attention.",
				review_context.run_id,
				self.issue.identifier,
				self.workflow.frontmatter().tracker().needs_attention_label()
			),
			(false, true, false) | (false, true, true) => eyre::bail!(
				"Run `{}` recorded a human-attention comment for issue `{}`, but never recorded label `{}`.",
				review_context.run_id,
				self.issue.identifier,
				self.workflow.frontmatter().tracker().needs_attention_label()
			),
		}
	}

	pub(crate) fn apply_review_handoff(&self) -> crate::prelude::Result<()> {
		let Some(review_context) = self.review_context.as_ref() else {
			eyre::bail!(
				"Review handoff context is unavailable for issue `{}`.",
				self.issue.identifier
			);
		};
		let pending_review_handoff = {
			let pending_review_handoff = self.pending_review_handoff.borrow();
			let Some(pending_review_handoff) = pending_review_handoff.as_ref() else {
				eyre::bail!(
					"Run `{}` completed, but issue `{}` never recorded a PR-backed review handoff.",
					review_context.run_id,
					self.issue.identifier
				);
			};

			pending_review_handoff.clone()
		};

		self.validate_review_handoff_pr(review_context, &pending_review_handoff.pr_url)
			.map_err(|error| eyre::eyre!(error))?;

		let completion_comment =
			format_review_handoff_comment(review_context, &pending_review_handoff);

		validate_public_comment_body(&completion_comment).map_err(|error| eyre::eyre!(error))?;

		let success_state = self.workflow.frontmatter().tracker().success_state();
		let success_state_id = self.issue.state_id_for_name(success_state).ok_or_else(|| {
			eyre::eyre!(
				"State `{success_state}` does not exist on issue `{}`.",
				self.issue.identifier
			)
		})?;

		self.tracker.update_issue_state(&self.issue.id, success_state_id)?;

		if let Err(error) = self.tracker.create_comment(&self.issue.id, &completion_comment) {
			return Err(Report::new(ReviewHandoffWritebackFailed {
				issue_identifier: self.issue.identifier.clone(),
				run_id: review_context.run_id.clone(),
				pr_url: pending_review_handoff.pr_url,
				success_state: success_state.to_owned(),
				source: error.to_string(),
			}));
		}

		self.pending_review_handoff.borrow_mut().take();

		Ok(())
	}
}

impl DynamicToolHandler for TrackerToolBridge<'_> {
	fn tool_specs(&self) -> Vec<DynamicToolSpec> {
		self.build_tool_specs()
	}

	fn handle_call(&self, tool_name: &str, arguments: Value) -> DynamicToolCallResponse {
		self.handle_call_inner(tool_name, arguments)
	}

	fn classify_turn_completion(
		&self,
		_final_output: &str,
	) -> crate::prelude::Result<TurnCompletionStatus> {
		let Some(review_context) = self.review_context.as_ref() else {
			eyre::bail!(
				"Review handoff context is unavailable for issue `{}`.",
				self.issue.identifier
			);
		};
		let manual_attention_requested = *self.manual_attention_requested.borrow();
		let manual_attention_comment_recorded = *self.manual_attention_comment_recorded.borrow();
		let review_handoff_recorded = self.pending_review_handoff.borrow().is_some();

		match (
			manual_attention_requested,
			manual_attention_comment_recorded,
			review_handoff_recorded,
		) {
			(false, false, false) => {
				if let Some(reason) = self.continuation_blocking_write_reason()? {
					eyre::bail!(
						"Run `{}` changed issue `{}` via {} without recording a terminal path. Continuation turns may only yield cleanly while the leased issue remains active.",
						review_context.run_id,
						self.issue.identifier,
						reason
					);
				}

				Ok(TurnCompletionStatus::Continue)
			},
			(false, false, true) | (true, true, false) => {
				self.validate_turn_completion("")?;

				Ok(TurnCompletionStatus::Complete)
			},
			(true, false, false) => eyre::bail!(
				"Run `{}` requested human attention with label `{}`, but issue `{}` never recorded the required explanatory comment.",
				review_context.run_id,
				self.workflow.frontmatter().tracker().needs_attention_label(),
				self.issue.identifier
			),
			(true, _, true) => eyre::bail!(
				"Run `{}` recorded both `issue_review_handoff` and label `{}`. Use exactly one final handoff path.",
				review_context.run_id,
				self.workflow.frontmatter().tracker().needs_attention_label()
			),
			(false, true, false) | (false, true, true) => eyre::bail!(
				"Run `{}` recorded a human-attention comment for issue `{}`, but never recorded label `{}`.",
				review_context.run_id,
				self.issue.identifier,
				self.workflow.frontmatter().tracker().needs_attention_label()
			),
		}
	}

	fn validate_turn_completion(&self, _final_output: &str) -> crate::prelude::Result<()> {
		let completion_path = self.completion_disposition()?;
		let Some(finalized_path) = *self.finalized_completion_path.borrow() else {
			let Some(review_context) = self.review_context.as_ref() else {
				eyre::bail!(
					"Review handoff context is unavailable for issue `{}`.",
					self.issue.identifier
				);
			};

			eyre::bail!(
				"Run `{}` completed, but issue `{}` never called `{}` for terminal path `{}`.",
				review_context.run_id,
				self.issue.identifier,
				ISSUE_TERMINAL_FINALIZE_TOOL_NAME,
				completion_path.as_str()
			);
		};

		if finalized_path != completion_path {
			let Some(review_context) = self.review_context.as_ref() else {
				eyre::bail!(
					"Review handoff context is unavailable for issue `{}`.",
					self.issue.identifier
				);
			};

			eyre::bail!(
				"Run `{}` finalized terminal path `{}`, but the recorded terminal path resolved to `{}` at turn completion.",
				review_context.run_id,
				finalized_path.as_str(),
				completion_path.as_str()
			);
		}

		Ok(())
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TurnCompletionStatus {
	Continue,
	Complete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewHandoffContext {
	pub(crate) attempt_number: i64,
	pub(crate) branch_name: String,
	pub(crate) run_id: String,
	pub(crate) workspace_path: String,
	pub(crate) cwd: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunCompletionDisposition {
	ManualAttention,
	ReviewHandoff,
}
impl RunCompletionDisposition {
	fn as_str(self) -> &'static str {
		match self {
			Self::ManualAttention => "manual_attention",
			Self::ReviewHandoff => "review_handoff",
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewHandoffWritebackFailed {
	pub(crate) issue_identifier: String,
	pub(crate) run_id: String,
	pub(crate) pr_url: String,
	pub(crate) success_state: String,
	pub(crate) source: String,
}
impl Display for ReviewHandoffWritebackFailed {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"Run `{}` moved issue `{}` to `{}` for PR `{}`, but failed to post the completion comment: {}",
			self.run_id, self.issue_identifier, self.success_state, self.pr_url, self.source
		)
	}
}
impl Error for ReviewHandoffWritebackFailed {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequestDetails {
	head_ref_name: String,
	head_ref_oid: String,
	head_repository_name: String,
	head_repository_owner: String,
	is_draft: bool,
	state: String,
	url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalRepoDetails {
	head_oid: String,
	repository_name: String,
	repository_owner: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DynamicToolCallResponse {
	#[serde(rename = "contentItems")]
	pub(crate) content_items: Vec<DynamicToolContentItem>,
	pub(crate) success: bool,
}
impl DynamicToolCallResponse {
	pub(crate) fn success(message: String) -> Self {
		Self { content_items: vec![DynamicToolContentItem::text(message)], success: true }
	}

	pub(crate) fn failure(message: String) -> Self {
		Self { content_items: vec![DynamicToolContentItem::text(message)], success: false }
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingReviewHandoff {
	pr_url: String,
	summary: String,
}

struct GhPullRequestInspector;
impl PullRequestInspector for GhPullRequestInspector {
	fn inspect_pull_request(
		&self,
		cwd: &std::path::Path,
		pr_url: &str,
	) -> std::result::Result<PullRequestDetails, String> {
		let output = Command::new("gh")
			.args([
				"pr",
				"view",
				pr_url,
				"--json",
				"url,headRefName,headRefOid,state,isDraft,headRepository,headRepositoryOwner",
			])
			.current_dir(cwd)
			.output()
			.map_err(|error| format!("Failed to inspect pull request `{pr_url}`: {error}"))?;

		if !output.status.success() {
			let stderr = String::from_utf8_lossy(&output.stderr);

			return Err(format!("Failed to inspect pull request `{pr_url}`: {}", stderr.trim()));
		}

		let response: PullRequestViewResponse =
			serde_json::from_slice(&output.stdout).map_err(|error| {
				format!("Failed to parse pull request details for `{pr_url}`: {error}")
			})?;
		let Some(head_repository) = response.head_repository else {
			return Err(format!(
				"Pull request `{pr_url}` does not expose a head repository for review handoff validation."
			));
		};

		Ok(PullRequestDetails {
			head_ref_name: response.head_ref_name,
			head_ref_oid: response.head_ref_oid,
			head_repository_name: head_repository.name,
			head_repository_owner: response.head_repository_owner.login,
			is_draft: response.is_draft,
			state: response.state,
			url: response.url,
		})
	}
}

struct LocalGitRepoInspector;
impl LocalRepoInspector for LocalGitRepoInspector {
	fn inspect_local_repo(
		&self,
		cwd: &std::path::Path,
	) -> std::result::Result<LocalRepoDetails, String> {
		let head_oid =
			run_command_for_stdout("git", &["rev-parse", "HEAD"], cwd, "inspect lane HEAD")?;
		let origin_url = run_command_for_stdout(
			"git",
			&["config", "--get", "remote.origin.url"],
			cwd,
			"inspect lane origin repository",
		)?;
		let repository = parse_github_repository_identity(origin_url.trim())?;

		Ok(LocalRepoDetails {
			head_oid,
			repository_name: repository.name,
			repository_owner: repository.owner,
		})
	}
}

#[derive(Debug, Deserialize)]
struct ScopeArgs {
	issue_id: Option<String>,

	issue_identifier: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionArgs {
	#[serde(flatten)]
	scope: ScopeArgs,
	state: String,
}

#[derive(Debug, Deserialize)]
struct CommentArgs {
	#[serde(flatten)]
	scope: ScopeArgs,
	body: String,
}

#[derive(Debug, Deserialize)]
struct ReviewHandoffArgs {
	#[serde(flatten)]
	scope: ScopeArgs,
	pr_url: String,
	summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelArgs {
	#[serde(flatten)]
	scope: ScopeArgs,
	label: String,
}

#[derive(Debug, Deserialize)]
struct TerminalFinalizeArgs {
	#[serde(flatten)]
	scope: ScopeArgs,
	path: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestViewResponse {
	#[serde(rename = "headRefName")]
	head_ref_name: String,
	#[serde(rename = "headRefOid")]
	head_ref_oid: String,
	#[serde(rename = "headRepository")]
	head_repository: Option<PullRequestRepositoryResponse>,
	#[serde(rename = "headRepositoryOwner")]
	head_repository_owner: PullRequestRepositoryOwnerResponse,
	#[serde(rename = "isDraft")]
	is_draft: bool,
	state: String,
	url: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestRepositoryResponse {
	name: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestRepositoryOwnerResponse {
	login: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub(crate) enum DynamicToolContentItem {
	#[serde(rename = "inputText")]
	InputText { text: String },
}
impl DynamicToolContentItem {
	fn text(text: String) -> Self {
		Self::InputText { text }
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RepositoryIdentity {
	name: String,
	owner: String,
}

fn run_command_for_stdout(
	command: &str,
	args: &[&str],
	cwd: &std::path::Path,
	purpose: &str,
) -> std::result::Result<String, String> {
	let output = Command::new(command)
		.args(args)
		.current_dir(cwd)
		.output()
		.map_err(|error| format!("Failed to {purpose} with `{command}`: {error}"))?;

	if !output.status.success() {
		let stderr = String::from_utf8_lossy(&output.stderr);
		let stdout = String::from_utf8_lossy(&output.stdout);
		let detail = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };

		if detail.is_empty() {
			return Err(format!("Failed to {purpose} with `{command}`."));
		}

		return Err(format!("Failed to {purpose} with `{command}`: {detail}"));
	}

	let stdout = String::from_utf8_lossy(&output.stdout);
	let value = stdout.trim();

	if value.is_empty() {
		return Err(format!("Failed to {purpose} with `{command}`: command returned no output."));
	}

	Ok(value.to_owned())
}

fn parse_github_repository_identity(
	remote_url: &str,
) -> std::result::Result<RepositoryIdentity, String> {
	let path = if let Some(path) = remote_url.strip_prefix("git@github.com:") {
		path
	} else {
		parse_github_remote_with_authority(remote_url)?
	};
	let path = path.strip_suffix(".git").unwrap_or(path);
	let mut parts = path.split('/');
	let Some(owner) = parts.next() else {
		return Err(format!("Unsupported GitHub remote URL `{remote_url}`."));
	};
	let Some(name) = parts.next() else {
		return Err(format!("Unsupported GitHub remote URL `{remote_url}`."));
	};

	if owner.is_empty() || name.is_empty() || parts.next().is_some() {
		return Err(format!("Unsupported GitHub remote URL `{remote_url}`."));
	}

	Ok(RepositoryIdentity { name: name.to_owned(), owner: owner.to_owned() })
}

fn parse_github_remote_with_authority(remote_url: &str) -> std::result::Result<&str, String> {
	let rest = remote_url
		.strip_prefix("https://")
		.or_else(|| remote_url.strip_prefix("http://"))
		.or_else(|| remote_url.strip_prefix("ssh://"))
		.ok_or_else(|| format!("Unsupported GitHub remote URL `{remote_url}`."))?;
	let (authority, path) = rest
		.split_once('/')
		.ok_or_else(|| format!("Unsupported GitHub remote URL `{remote_url}`."))?;
	let authority = authority.rsplit('@').next().unwrap_or(authority);
	let host = authority.split_once(':').map(|(host, _)| host).unwrap_or(authority);

	if host != "github.com" {
		return Err(format!("Unsupported GitHub remote URL `{remote_url}`."));
	}

	Ok(path)
}

fn validate_public_comment_body(body: &str) -> Result<(), String> {
	for line in body.lines() {
		let Some(workspace_path) = extract_structured_field_value(line, "workspace_path") else {
			continue;
		};

		validate_repo_relative_path(workspace_path)?;
	}

	Ok(())
}

fn extract_structured_field_value<'a>(line: &'a str, field_name: &str) -> Option<&'a str> {
	let trimmed = line.trim();
	let trimmed = trimmed.strip_prefix("- ").unwrap_or(trimmed);
	let (key, value) = trimmed.split_once(':')?;

	(key.trim() == field_name).then_some(value.trim().trim_matches('`'))
}

fn validate_repo_relative_path(path: &str) -> Result<(), String> {
	if path.is_empty() {
		return Err(String::from("`workspace_path` must not be empty."));
	}
	if path.starts_with('/') || path.starts_with("~/") || is_windows_absolute_path(path) {
		return Err(format!("`workspace_path` must be repository-relative, not `{path}`."));
	}

	let components = std::path::Path::new(path).components();

	if components.into_iter().any(|component| matches!(component, Component::ParentDir)) {
		return Err(format!("`workspace_path` must stay within the repository, not `{path}`."));
	}

	Ok(())
}

fn is_windows_absolute_path(path: &str) -> bool {
	let bytes = path.as_bytes();

	bytes.len() >= 3
		&& bytes[0].is_ascii_alphabetic()
		&& bytes[1] == b':'
		&& matches!(bytes[2], b'\\' | b'/')
}

fn normalize_summary(summary: &str) -> String {
	summary.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_review_handoff_comment(
	review_context: &ReviewHandoffContext,
	pending_review_handoff: &PendingReviewHandoff,
) -> String {
	format!(
		"maestro run completed and is ready for review\n\n- run_id: `{run_id}`\n- attempt: `{attempt}`\n- finished_at: `{finished_at}`\n- branch: `{branch}`\n- pr_url: `{pr_url}`\n- workspace_path: `{workspace_path}`\n- validation_result: `passed`\n- summary: {summary}",
		run_id = review_context.run_id,
		attempt = review_context.attempt_number,
		finished_at = current_timestamp(),
		branch = review_context.branch_name,
		pr_url = pending_review_handoff.pr_url,
		workspace_path = review_context.workspace_path,
		summary = pending_review_handoff.summary,
	)
}

fn current_timestamp() -> String {
	OffsetDateTime::now_utc().format(&Rfc3339).expect("timestamp formatting should succeed")
}

#[cfg(test)]
mod tests {
	use std::{
		cell::RefCell,
		path::{Path, PathBuf},
	};

	use crate::{
		agent::tracker_tool_bridge::{
			DynamicToolHandler, ISSUE_COMMENT_TOOL_NAME, ISSUE_LABEL_ADD_TOOL_NAME,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME, ISSUE_TERMINAL_FINALIZE_TOOL_NAME,
			ISSUE_TRANSITION_TOOL_NAME, LocalRepoDetails, LocalRepoInspector, PullRequestDetails,
			PullRequestInspector, ReviewHandoffContext, RunCompletionDisposition,
			TrackerToolBridge, TurnCompletionStatus,
		},
		prelude::{Result, eyre},
		tracker::{
			IssueTracker, TrackerIssue, TrackerLabel, TrackerProject, TrackerState, TrackerTeam,
		},
		workflow::WorkflowDocument,
	};

	struct FakeTracker {
		state_updates: RefCell<Vec<String>>,
		label_updates: RefCell<Vec<Vec<String>>>,
		comments: RefCell<Vec<String>>,
		refresh_snapshots: RefCell<Vec<Vec<TrackerIssue>>>,
		fail_state_update: RefCell<Option<String>>,
		fail_label_update: RefCell<Option<String>>,
		fail_comment: RefCell<Option<String>>,
	}

	struct FakePullRequestInspector {
		responses: RefCell<Vec<std::result::Result<PullRequestDetails, String>>>,
	}
	impl FakePullRequestInspector {
		fn new(responses: Vec<std::result::Result<PullRequestDetails, String>>) -> Self {
			Self { responses: RefCell::new(responses) }
		}
	}
	impl PullRequestInspector for FakePullRequestInspector {
		fn inspect_pull_request(
			&self,
			_cwd: &Path,
			_pr_url: &str,
		) -> std::result::Result<PullRequestDetails, String> {
			self.responses.borrow_mut().remove(0)
		}
	}
	struct FakeLocalRepoInspector {
		responses: RefCell<Vec<std::result::Result<LocalRepoDetails, String>>>,
	}
	impl FakeLocalRepoInspector {
		fn new(responses: Vec<std::result::Result<LocalRepoDetails, String>>) -> Self {
			Self { responses: RefCell::new(responses) }
		}
	}
	impl LocalRepoInspector for FakeLocalRepoInspector {
		fn inspect_local_repo(&self, _cwd: &Path) -> std::result::Result<LocalRepoDetails, String> {
			self.responses.borrow_mut().remove(0)
		}
	}
	impl FakeTracker {
		fn new() -> Self {
			Self {
				state_updates: RefCell::new(Vec::new()),
				label_updates: RefCell::new(Vec::new()),
				comments: RefCell::new(Vec::new()),
				refresh_snapshots: RefCell::new(Vec::new()),
				fail_state_update: RefCell::new(None),
				fail_label_update: RefCell::new(None),
				fail_comment: RefCell::new(None),
			}
		}

		fn with_refresh_snapshots(refresh_snapshots: Vec<Vec<TrackerIssue>>) -> Self {
			let tracker = Self::new();

			tracker.refresh_snapshots.replace(refresh_snapshots);

			tracker
		}

		fn with_state_update_error(message: &str) -> Self {
			let tracker = Self::new();

			tracker.fail_state_update.replace(Some(message.to_owned()));

			tracker
		}

		fn with_label_update_error(message: &str) -> Self {
			let tracker = Self::new();

			tracker.fail_label_update.replace(Some(message.to_owned()));

			tracker
		}

		fn with_comment_error(message: &str) -> Self {
			let tracker = Self::new();

			tracker.fail_comment.replace(Some(message.to_owned()));

			tracker
		}
	}
	impl IssueTracker for FakeTracker {
		fn list_project_issues(&self, _project_slug: &str) -> Result<Vec<TrackerIssue>> {
			Ok(Vec::new())
		}

		fn get_project_by_slug(&self, _project_slug: &str) -> Result<Option<TrackerProject>> {
			Ok(None)
		}

		fn refresh_issues(&self, _issue_ids: &[String]) -> Result<Vec<TrackerIssue>> {
			if self.refresh_snapshots.borrow().is_empty() {
				return Ok(Vec::new());
			}

			Ok(self.refresh_snapshots.borrow_mut().remove(0))
		}

		fn update_issue_state(&self, _issue_id: &str, state_id: &str) -> Result<()> {
			if let Some(message) = self.fail_state_update.borrow().as_ref() {
				return Err(eyre::eyre!(message.clone()));
			}

			self.state_updates.borrow_mut().push(state_id.to_owned());

			Ok(())
		}

		fn update_issue_labels(&self, _issue_id: &str, label_ids: &[String]) -> Result<()> {
			if let Some(message) = self.fail_label_update.borrow().as_ref() {
				return Err(eyre::eyre!(message.clone()));
			}

			self.label_updates.borrow_mut().push(label_ids.to_vec());

			Ok(())
		}

		fn create_comment(&self, _issue_id: &str, body: &str) -> Result<()> {
			if let Some(message) = self.fail_comment.borrow().as_ref() {
				return Err(eyre::eyre!(message.clone()));
			}

			self.comments.borrow_mut().push(body.to_owned());

			Ok(())
		}
	}

	fn sample_issue() -> TrackerIssue {
		TrackerIssue {
			id: String::from("issue-1"),
			identifier: String::from("MAE-1"),
			project_slug: Some(String::from("maestro")),
			title: String::from("Sample"),
			description: String::from("Body"),
			priority: Some(3),
			created_at: String::from("2026-03-13T04:16:17.133Z"),
			state: TrackerState { id: String::from("state-todo"), name: String::from("Todo") },
			team: TrackerTeam {
				id: String::from("team-1"),
				name: String::from("Maestro"),
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
				labels: vec![
					TrackerLabel {
						id: String::from("label-manual"),
						name: String::from("maestro:manual-only"),
					},
					TrackerLabel {
						id: String::from("label-needs"),
						name: String::from("maestro:needs-attention"),
					},
				],
			},
			labels: Vec::new(),
			blockers: Vec::new(),
		}
	}

	fn sample_in_progress_issue() -> TrackerIssue {
		let mut issue = sample_issue();

		issue.state =
			TrackerState { id: String::from("state-progress"), name: String::from("In Progress") };

		issue
	}

	fn tracker_with_current_issue_snapshot(issue: &TrackerIssue) -> FakeTracker {
		FakeTracker::with_refresh_snapshots(vec![vec![issue.clone()]])
	}

	fn sample_workflow() -> WorkflowDocument {
		sample_workflow_with_tracker_states(&["Todo"], "In Progress", "In Review", "Todo")
	}

	fn sample_workflow_with_startable_states(startable_states: &[&str]) -> WorkflowDocument {
		sample_workflow_with_tracker_states(startable_states, "In Progress", "In Review", "Todo")
	}

	fn sample_workflow_with_tracker_states(
		startable_states: &[&str],
		in_progress_state: &str,
		success_state: &str,
		failure_state: &str,
	) -> WorkflowDocument {
		let startable_states = startable_states
			.iter()
			.map(|state| format!("\"{state}\""))
			.collect::<Vec<_>>()
			.join(", ");

		WorkflowDocument::parse_markdown(&format!(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "maestro"
startable_states = [{startable_states}]
in_progress_state = "{in_progress_state}"
success_state = "{success_state}"
failure_state = "{failure_state}"
opt_out_label = "maestro:manual-only"
needs_attention_label = "maestro:needs-attention"
+++

Use the tracker tools.
"#,
		))
		.expect("workflow should parse")
	}

	fn sample_review_context() -> ReviewHandoffContext {
		ReviewHandoffContext {
			attempt_number: 2,
			branch_name: String::from("x/maestro-pub-618"),
			run_id: String::from("pub-618-attempt-2-123"),
			workspace_path: String::from(".workspaces/PUB-618"),
			cwd: PathBuf::from("/tmp/PUB-618"),
		}
	}

	fn sample_local_repo() -> LocalRepoDetails {
		LocalRepoDetails {
			head_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
			repository_name: String::from("maestro"),
			repository_owner: String::from("helixbox"),
		}
	}

	#[test]
	fn transitions_current_issue_with_allowed_state() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TRANSITION_TOOL_NAME,
			serde_json::json!({ "issue_identifier": "MAE-1", "state": "In Progress" }),
		);

		assert!(response.success);
		assert_eq!(tracker.state_updates.borrow().as_slice(), ["state-progress"]);
	}

	#[test]
	fn rejects_success_transition_without_review_handoff() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TRANSITION_TOOL_NAME,
			serde_json::json!({ "state": "In Review" }),
		);

		assert!(!response.success);
		assert!(tracker.state_updates.borrow().is_empty());
		assert_eq!(
			response.content_items,
			vec![super::DynamicToolContentItem::InputText {
				text: String::from(
					"State `In Review` requires `issue_review_handoff` after the branch is pushed and a reviewable PR exists."
				),
			}]
		);
	}

	#[test]
	fn rejects_legacy_state_name_argument() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TRANSITION_TOOL_NAME,
			serde_json::json!({ "state_name": "In Progress" }),
		);

		assert!(!response.success);
		assert!(tracker.state_updates.borrow().is_empty());
		assert_eq!(
			response.content_items,
			vec![super::DynamicToolContentItem::InputText {
				text: String::from("Invalid `issue.transition` arguments: missing field `state`"),
			}]
		);
	}

	#[test]
	fn rejects_legacy_state_name_argument_when_state_is_present() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TRANSITION_TOOL_NAME,
			serde_json::json!({ "state": "In Progress", "state_name": "In Progress" }),
		);

		assert!(!response.success);
		assert!(tracker.state_updates.borrow().is_empty());
		assert_eq!(
			response.content_items,
			vec![super::DynamicToolContentItem::InputText {
				text: String::from(
					"Invalid `issue.transition` arguments: unknown field `state_name`"
				),
			}]
		);
	}

	#[test]
	fn rejects_success_transition_even_when_success_state_is_startable() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow_with_startable_states(&["Todo", "In Review"]);

		assert_success_transition_requires_review_handoff(workflow, &tracker, &issue);
	}

	#[test]
	fn rejects_success_transition_even_when_failure_state_matches_success_state() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow =
			sample_workflow_with_tracker_states(&["Todo"], "In Progress", "In Review", "In Review");

		assert_success_transition_requires_review_handoff(workflow, &tracker, &issue);
	}

	#[test]
	fn rejects_success_transition_even_when_in_progress_state_matches_success_state() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow =
			sample_workflow_with_tracker_states(&["Todo"], "In Review", "In Review", "Todo");

		assert_success_transition_requires_review_handoff(workflow, &tracker, &issue);
	}

	fn assert_success_transition_requires_review_handoff(
		workflow: WorkflowDocument,
		tracker: &FakeTracker,
		issue: &TrackerIssue,
	) {
		let bridge = TrackerToolBridge::new(tracker, issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TRANSITION_TOOL_NAME,
			serde_json::json!({ "state": "In Review" }),
		);

		assert!(!response.success);
		assert!(tracker.state_updates.borrow().is_empty());
		assert_eq!(
			response.content_items,
			vec![super::DynamicToolContentItem::InputText {
				text: String::from(
					"State `In Review` requires `issue_review_handoff` after the branch is pushed and a reviewable PR exists."
				),
			}]
		);
	}

	#[test]
	fn rejects_tool_calls_for_another_issue() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_COMMENT_TOOL_NAME,
			serde_json::json!({ "issue_identifier": "MAE-999", "body": "hello" }),
		);

		assert!(!response.success);
		assert!(tracker.comments.borrow().is_empty());
	}

	#[test]
	fn accepts_comment_without_structured_workspace_path() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_COMMENT_TOOL_NAME,
			serde_json::json!({ "body": "Started work and running validation now." }),
		);

		assert!(response.success);
		assert_eq!(
			tracker.comments.borrow().as_slice(),
			["Started work and running validation now."]
		);
	}

	#[test]
	fn accepts_repo_relative_workspace_path_in_comment_body() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_COMMENT_TOOL_NAME,
			serde_json::json!({
				"body": "maestro run failed and will retry\n\n- workspace_path: `.workspaces/MAE-1`"
			}),
		);

		assert!(response.success);
		assert_eq!(
			tracker.comments.borrow().as_slice(),
			["maestro run failed and will retry\n\n- workspace_path: `.workspaces/MAE-1`"]
		);
	}

	#[test]
	fn rejects_absolute_workspace_path_in_comment_body() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_COMMENT_TOOL_NAME,
			serde_json::json!({
				"body": "maestro run failed and will retry\n\n- workspace_path: `/absolute/path/to/repo/.workspaces/MAE-1`"
			}),
		);

		assert!(!response.success);
		assert!(tracker.comments.borrow().is_empty());
		assert_eq!(
			response.content_items,
			vec![super::DynamicToolContentItem::InputText {
				text: String::from(
					"`workspace_path` must be repository-relative, not `/absolute/path/to/repo/.workspaces/MAE-1`."
				),
			}]
		);
	}

	#[test]
	fn rejects_windows_absolute_workspace_path_in_comment_body() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_COMMENT_TOOL_NAME,
			serde_json::json!({
				"body": "maestro run failed and will retry\n\n- workspace_path: `C:/absolute/path/to/repo/.workspaces/MAE-1`"
			}),
		);

		assert!(!response.success);
		assert!(tracker.comments.borrow().is_empty());
		assert_eq!(
			response.content_items,
			vec![super::DynamicToolContentItem::InputText {
				text: String::from(
					"`workspace_path` must be repository-relative, not `C:/absolute/path/to/repo/.workspaces/MAE-1`."
				),
			}]
		);
	}

	#[test]
	fn adds_allowed_workflow_label() {
		let issue = sample_issue();
		let tracker = tracker_with_current_issue_snapshot(&issue);
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:needs-attention" }),
		);

		assert!(response.success);
		assert_eq!(tracker.label_updates.borrow().as_slice(), [vec![String::from("label-needs")]]);
	}

	#[test]
	fn rejects_legacy_label_name_argument() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label_name": "maestro:needs-attention" }),
		);

		assert!(!response.success);
		assert!(tracker.label_updates.borrow().is_empty());
		assert_eq!(
			response.content_items,
			vec![super::DynamicToolContentItem::InputText {
				text: String::from("Invalid `issue.label.add` arguments: missing field `label`"),
			}]
		);
	}

	#[test]
	fn rejects_legacy_label_name_argument_when_label_is_present() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({
				"label": "maestro:needs-attention",
				"label_name": "maestro:needs-attention"
			}),
		);

		assert!(!response.success);
		assert!(tracker.label_updates.borrow().is_empty());
		assert_eq!(
			response.content_items,
			vec![super::DynamicToolContentItem::InputText {
				text: String::from(
					"Invalid `issue.label.add` arguments: unknown field `label_name`"
				),
			}]
		);
	}

	#[test]
	fn completion_disposition_allows_manual_attention_exit_without_review_handoff() {
		let issue = sample_issue();
		let tracker = tracker_with_current_issue_snapshot(&issue);
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:needs-attention" }),
		);
		let comment_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_COMMENT_TOOL_NAME,
			serde_json::json!({ "body": "Blocked on missing tracker permission; handing off for manual repair." }),
		);

		assert!(response.success);
		assert!(comment_response.success);
		assert_eq!(
			bridge.completion_disposition().expect("manual attention should be accepted"),
			RunCompletionDisposition::ManualAttention
		);
	}

	#[test]
	fn turn_completion_rejects_xy_156_shape_without_terminal_tracker_action() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let error = DynamicToolHandler::validate_turn_completion(
			&bridge,
			"Implementation and tests are done, but commit, push, PR, and tracker handoff remain.",
		)
		.expect_err("turn completion should reject missing terminal tracker actions");

		assert!(error.to_string().contains("recorded neither a PR-backed review handoff"));
	}

	#[test]
	fn turn_classification_allows_continuation_without_terminal_tracker_action() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);

		assert_eq!(
			DynamicToolHandler::classify_turn_completion(
				&bridge,
				"Still implementing; no terminal tracker action has been recorded yet."
			)
			.expect("missing terminal action should request continuation"),
			TurnCompletionStatus::Continue
		);
	}

	#[test]
	fn turn_classification_rejects_opt_out_label_without_terminal_path() {
		let mut opted_out_issue = sample_issue();

		opted_out_issue.labels.push(TrackerLabel {
			id: String::from("label-manual"),
			name: String::from("maestro:manual-only"),
		});

		let tracker = FakeTracker::with_refresh_snapshots(vec![vec![opted_out_issue]]);
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:manual-only" }),
		);

		assert!(response.success);

		let error = DynamicToolHandler::classify_turn_completion(
			&bridge,
			"The lane opted out, but no terminal path was recorded.",
		)
		.expect_err("opt-out writes must not exit via a clean continuation boundary");

		assert!(error.to_string().contains("without recording a terminal path"));
		assert!(error.to_string().contains(ISSUE_LABEL_ADD_TOOL_NAME));
	}

	#[test]
	fn turn_classification_rejects_non_in_progress_transition_without_terminal_path() {
		let mut todo_issue = sample_issue();

		todo_issue.state =
			TrackerState { id: String::from("state-todo"), name: String::from("Todo") };

		let tracker = FakeTracker::with_refresh_snapshots(vec![vec![todo_issue]]);
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TRANSITION_TOOL_NAME,
			serde_json::json!({ "state": "Todo" }),
		);

		assert!(response.success);

		let error = DynamicToolHandler::classify_turn_completion(
			&bridge,
			"The issue moved back to Todo without a terminal path.",
		)
		.expect_err("non-In-Progress transitions must not exit via a clean continuation boundary");

		assert!(error.to_string().contains("without recording a terminal path"));
		assert!(error.to_string().contains(ISSUE_TRANSITION_TOOL_NAME));
	}

	#[test]
	fn turn_classification_allows_opt_out_label_when_refresh_reactivates_issue() {
		let active_issue = sample_in_progress_issue();
		let tracker = FakeTracker::with_refresh_snapshots(vec![
			vec![active_issue.clone()],
			vec![active_issue],
		]);
		let issue = sample_in_progress_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:manual-only" }),
		);

		assert!(response.success);
		assert_eq!(
			DynamicToolHandler::classify_turn_completion(
				&bridge,
				"Tracker reread reactivated the issue, so the continuation block should clear."
			)
			.expect("fresh active rereads should clear an opt-out continuation block"),
			TurnCompletionStatus::Continue
		);
	}

	#[test]
	fn turn_classification_allows_non_in_progress_transition_when_refresh_reactivates_issue() {
		let active_issue = sample_in_progress_issue();
		let tracker = FakeTracker::with_refresh_snapshots(vec![
			vec![active_issue.clone()],
			vec![active_issue],
		]);
		let issue = sample_in_progress_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TRANSITION_TOOL_NAME,
			serde_json::json!({ "state": "Todo" }),
		);

		assert!(response.success);
		assert_eq!(
			DynamicToolHandler::classify_turn_completion(
				&bridge,
				"Tracker reread reactivated the issue, so the local non-active transition no longer blocks continuation."
			)
			.expect("fresh active rereads should clear a local non-active transition block"),
			TurnCompletionStatus::Continue
		);
	}

	#[test]
	fn manual_attention_requires_explanatory_comment() {
		let issue = sample_issue();
		let tracker = tracker_with_current_issue_snapshot(&issue);
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:needs-attention" }),
		);

		assert!(response.success);

		let error = bridge
			.completion_disposition()
			.expect_err("manual attention must require an explanatory comment");

		assert!(error.to_string().contains("never recorded the required explanatory comment"));
	}

	#[test]
	fn failed_needs_attention_label_update_does_not_record_manual_attention() {
		let tracker = FakeTracker::with_label_update_error("tracker labels unavailable");
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:needs-attention" }),
		);

		assert!(!response.success);
		assert!(tracker.label_updates.borrow().is_empty());

		let error = bridge
			.completion_disposition()
			.expect_err("failed label writes must not count as manual attention");

		assert!(error.to_string().contains("recorded neither"));
	}

	#[test]
	fn label_add_refreshes_issue_snapshot_before_merging_label_ids() {
		let initial_issue = sample_issue();
		let mut refreshed_issue = initial_issue.clone();

		refreshed_issue.labels.push(TrackerLabel {
			id: String::from("label-manual"),
			name: String::from("maestro:manual-only"),
		});

		let tracker = FakeTracker::with_refresh_snapshots(vec![vec![refreshed_issue]]);
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &initial_issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:needs-attention" }),
		);

		assert!(response.success);
		assert_eq!(
			tracker.label_updates.borrow().as_slice(),
			[vec![String::from("label-manual"), String::from("label-needs")]]
		);
	}

	#[test]
	fn label_add_fails_when_refresh_returns_no_snapshot() {
		let tracker = FakeTracker::with_refresh_snapshots(vec![Vec::new()]);
		let workflow = sample_workflow();
		let issue = sample_issue();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:needs-attention" }),
		);

		assert!(!response.success);
		assert_eq!(
			response.content_items,
			vec![super::DynamicToolContentItem::InputText {
				text: format!(
					"Failed to refresh issue `{}` before updating labels: tracker returned no current snapshot.",
					issue.identifier
				),
			}]
		);
		assert!(tracker.label_updates.borrow().is_empty());
	}

	#[test]
	fn turn_classification_rejects_continuation_blocking_write_when_refresh_returns_no_snapshot() {
		let mut opted_out_issue = sample_issue();

		opted_out_issue.labels.push(TrackerLabel {
			id: String::from("label-manual"),
			name: String::from("maestro:manual-only"),
		});

		let tracker = FakeTracker::with_refresh_snapshots(vec![vec![opted_out_issue], Vec::new()]);
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:manual-only" }),
		);

		assert!(response.success);

		let error = DynamicToolHandler::classify_turn_completion(
			&bridge,
			"The lane recorded a continuation-blocking tracker write without a terminal path.",
		)
		.expect_err("missing refresh snapshots must not allow a clean continuation boundary");

		assert!(error.to_string().contains("without recording a terminal path"));
		assert!(error.to_string().contains(ISSUE_LABEL_ADD_TOOL_NAME));
	}

	#[test]
	fn completion_disposition_rejects_conflicting_review_handoff_and_manual_attention() {
		let issue = sample_issue();
		let tracker = tracker_with_current_issue_snapshot(&issue);
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![Ok(PullRequestDetails {
			head_ref_name: String::from("x/maestro-pub-618"),
			head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
			head_repository_name: String::from("maestro"),
			head_repository_owner: String::from("helixbox"),
			is_draft: false,
			state: String::from("OPEN"),
			url: String::from("https://github.com/helixbox/maestro/pull/48"),
		})]);
		let local_repo_inspector = FakeLocalRepoInspector::new(vec![Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let review_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/48",
				"summary": "Ready for review."
			}),
		);
		let label_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:needs-attention" }),
		);

		assert!(review_response.success);
		assert!(label_response.success);

		let error = bridge
			.completion_disposition()
			.expect_err("conflicting completion signals should be rejected");

		assert!(error.to_string().contains("Use exactly one final handoff path."));
	}

	#[test]
	fn records_review_handoff_and_applies_it_after_validation() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![
			Ok(PullRequestDetails {
				head_ref_name: String::from("x/maestro-pub-618"),
				head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
				head_repository_name: String::from("maestro"),
				head_repository_owner: String::from("helixbox"),
				is_draft: false,
				state: String::from("OPEN"),
				url: String::from("https://github.com/helixbox/maestro/pull/42"),
			}),
			Ok(PullRequestDetails {
				head_ref_name: String::from("x/maestro-pub-618"),
				head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
				head_repository_name: String::from("maestro"),
				head_repository_owner: String::from("helixbox"),
				is_draft: false,
				state: String::from("OPEN"),
				url: String::from("https://github.com/helixbox/maestro/pull/42"),
			}),
		]);
		let local_repo_inspector =
			FakeLocalRepoInspector::new(vec![Ok(sample_local_repo()), Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/42",
				"summary": "Implemented the PR-backed review handoff."
			}),
		);

		assert!(response.success);

		bridge.apply_review_handoff().expect("review handoff should apply");

		assert_eq!(tracker.state_updates.borrow().as_slice(), ["state-review"]);

		let comments = tracker.comments.borrow();

		assert_eq!(comments.len(), 1);
		assert!(comments[0].contains("- pr_url: `https://github.com/helixbox/maestro/pull/42`"));
		assert!(comments[0].contains("- validation_result: `passed`"));
		assert!(comments[0].contains("- workspace_path: `.workspaces/PUB-618`"));
	}

	#[test]
	fn turn_completion_requires_explicit_terminal_finalize_after_review_handoff() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![Ok(PullRequestDetails {
			head_ref_name: String::from("x/maestro-pub-618"),
			head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
			head_repository_name: String::from("maestro"),
			head_repository_owner: String::from("helixbox"),
			is_draft: false,
			state: String::from("OPEN"),
			url: String::from("https://github.com/helixbox/maestro/pull/52"),
		})]);
		let local_repo_inspector = FakeLocalRepoInspector::new(vec![Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/52",
				"summary": "Ready for review."
			}),
		);

		assert!(response.success);

		let error = DynamicToolHandler::validate_turn_completion(&bridge, "done")
			.expect_err("review handoff should still require explicit finalization");

		assert!(error.to_string().contains(ISSUE_TERMINAL_FINALIZE_TOOL_NAME));
		assert!(error.to_string().contains("review_handoff"));
	}

	#[test]
	fn terminal_finalize_accepts_matching_review_handoff_path() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![Ok(PullRequestDetails {
			head_ref_name: String::from("x/maestro-pub-618"),
			head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
			head_repository_name: String::from("maestro"),
			head_repository_owner: String::from("helixbox"),
			is_draft: false,
			state: String::from("OPEN"),
			url: String::from("https://github.com/helixbox/maestro/pull/53"),
		})]);
		let local_repo_inspector = FakeLocalRepoInspector::new(vec![Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let review_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/53",
				"summary": "Ready for review."
			}),
		);
		let finalize_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TERMINAL_FINALIZE_TOOL_NAME,
			serde_json::json!({ "path": "review_handoff" }),
		);

		assert!(review_response.success);
		assert!(finalize_response.success);

		DynamicToolHandler::validate_turn_completion(&bridge, "done")
			.expect("matching finalization should allow the turn to complete");
	}

	#[test]
	fn terminal_finalize_rejects_mismatched_path() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![Ok(PullRequestDetails {
			head_ref_name: String::from("x/maestro-pub-618"),
			head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
			head_repository_name: String::from("maestro"),
			head_repository_owner: String::from("helixbox"),
			is_draft: false,
			state: String::from("OPEN"),
			url: String::from("https://github.com/helixbox/maestro/pull/54"),
		})]);
		let local_repo_inspector = FakeLocalRepoInspector::new(vec![Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let review_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/54",
				"summary": "Ready for review."
			}),
		);
		let finalize_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TERMINAL_FINALIZE_TOOL_NAME,
			serde_json::json!({ "path": "manual_attention" }),
		);

		assert!(review_response.success);
		assert!(!finalize_response.success);
		assert!(matches!(
			finalize_response.content_items.as_slice(),
			[crate::agent::tracker_tool_bridge::DynamicToolContentItem::InputText { text }]
				if text.contains(
					"requested path `manual_attention`, but the recorded terminal path is `review_handoff`"
				)
		));
	}

	#[test]
	fn terminal_finalize_accepts_matching_manual_attention_path() {
		let issue = sample_issue();
		let tracker = tracker_with_current_issue_snapshot(&issue);
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let local_repo_inspector = FakeLocalRepoInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let label_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_LABEL_ADD_TOOL_NAME,
			serde_json::json!({ "label": "maestro:needs-attention" }),
		);
		let comment_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_COMMENT_TOOL_NAME,
			serde_json::json!({
				"body": "Blocked on missing tracker permission; handing off for manual repair."
			}),
		);
		let finalize_response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_TERMINAL_FINALIZE_TOOL_NAME,
			serde_json::json!({ "path": "manual_attention" }),
		);

		assert!(label_response.success);
		assert!(comment_response.success);
		assert!(finalize_response.success);

		DynamicToolHandler::validate_turn_completion(&bridge, "done")
			.expect("matching manual-attention finalization should allow the turn to complete");
	}

	#[test]
	fn rejects_review_handoff_apply_when_lane_head_changes_after_recording() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![
			Ok(PullRequestDetails {
				head_ref_name: String::from("x/maestro-pub-618"),
				head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
				head_repository_name: String::from("maestro"),
				head_repository_owner: String::from("helixbox"),
				is_draft: false,
				state: String::from("OPEN"),
				url: String::from("https://github.com/helixbox/maestro/pull/47"),
			}),
			Ok(PullRequestDetails {
				head_ref_name: String::from("x/maestro-pub-618"),
				head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
				head_repository_name: String::from("maestro"),
				head_repository_owner: String::from("helixbox"),
				is_draft: false,
				state: String::from("OPEN"),
				url: String::from("https://github.com/helixbox/maestro/pull/47"),
			}),
		]);
		let mut updated_local_repo = sample_local_repo();

		updated_local_repo.head_oid = String::from("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");

		let local_repo_inspector =
			FakeLocalRepoInspector::new(vec![Ok(sample_local_repo()), Ok(updated_local_repo)]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/47",
				"summary": "Ready for review."
			}),
		);

		assert!(response.success);

		let error = bridge
			.apply_review_handoff()
			.expect_err("writeback should revalidate the current lane head");

		assert!(error.to_string().contains("Push the latest lane commit before review handoff."));
		assert!(tracker.comments.borrow().is_empty());
		assert!(tracker.state_updates.borrow().is_empty());
	}

	#[test]
	fn rejects_review_handoff_before_posting_success_comment_when_state_transition_fails() {
		let tracker = FakeTracker::with_state_update_error("tracker state write failed");
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![
			Ok(PullRequestDetails {
				head_ref_name: String::from("x/maestro-pub-618"),
				head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
				head_repository_name: String::from("maestro"),
				head_repository_owner: String::from("helixbox"),
				is_draft: false,
				state: String::from("OPEN"),
				url: String::from("https://github.com/helixbox/maestro/pull/49"),
			}),
			Ok(PullRequestDetails {
				head_ref_name: String::from("x/maestro-pub-618"),
				head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
				head_repository_name: String::from("maestro"),
				head_repository_owner: String::from("helixbox"),
				is_draft: false,
				state: String::from("OPEN"),
				url: String::from("https://github.com/helixbox/maestro/pull/49"),
			}),
		]);
		let local_repo_inspector =
			FakeLocalRepoInspector::new(vec![Ok(sample_local_repo()), Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/49",
				"summary": "Ready for review."
			}),
		);

		assert!(response.success);

		let error = bridge
			.apply_review_handoff()
			.expect_err("state transition failures must block the success comment");

		assert!(error.to_string().contains("tracker state write failed"));
		assert!(tracker.comments.borrow().is_empty());
		assert!(tracker.state_updates.borrow().is_empty());
	}

	#[test]
	fn reports_partial_review_handoff_when_comment_write_fails_after_state_update() {
		let tracker = FakeTracker::with_comment_error("tracker comment write failed");
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![
			Ok(PullRequestDetails {
				head_ref_name: String::from("x/maestro-pub-618"),
				head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
				head_repository_name: String::from("maestro"),
				head_repository_owner: String::from("helixbox"),
				is_draft: false,
				state: String::from("OPEN"),
				url: String::from("https://github.com/helixbox/maestro/pull/50"),
			}),
			Ok(PullRequestDetails {
				head_ref_name: String::from("x/maestro-pub-618"),
				head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
				head_repository_name: String::from("maestro"),
				head_repository_owner: String::from("helixbox"),
				is_draft: false,
				state: String::from("OPEN"),
				url: String::from("https://github.com/helixbox/maestro/pull/50"),
			}),
		]);
		let local_repo_inspector =
			FakeLocalRepoInspector::new(vec![Ok(sample_local_repo()), Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/50",
				"summary": "Ready for review."
			}),
		);

		assert!(response.success);

		let error = bridge.apply_review_handoff().expect_err(
			"comment write failures after state transition must surface as partial writeback",
		);
		let writeback_error = error
			.downcast_ref::<super::ReviewHandoffWritebackFailed>()
			.expect("partial writeback should use dedicated error type");

		assert_eq!(writeback_error.issue_identifier, "MAE-1");
		assert_eq!(writeback_error.run_id, "pub-618-attempt-2-123");
		assert_eq!(writeback_error.success_state, "In Review");
		assert!(writeback_error.source.contains("tracker comment write failed"));
		assert_eq!(tracker.state_updates.borrow().as_slice(), ["state-review"]);
		assert!(tracker.comments.borrow().is_empty());
	}

	#[test]
	fn rejects_review_handoff_for_another_branch() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![Ok(PullRequestDetails {
			head_ref_name: String::from("x/maestro-pub-999"),
			head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
			head_repository_name: String::from("maestro"),
			head_repository_owner: String::from("helixbox"),
			is_draft: false,
			state: String::from("OPEN"),
			url: String::from("https://github.com/helixbox/maestro/pull/43"),
		})]);
		let local_repo_inspector = FakeLocalRepoInspector::new(vec![Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/43",
				"summary": "Ready for review."
			}),
		);

		assert!(!response.success);
		assert!(tracker.comments.borrow().is_empty());
		assert!(bridge.apply_review_handoff().is_err());
	}

	#[test]
	fn rejects_draft_pull_requests_for_review_handoff() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![Ok(PullRequestDetails {
			head_ref_name: String::from("x/maestro-pub-618"),
			head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
			head_repository_name: String::from("maestro"),
			head_repository_owner: String::from("helixbox"),
			is_draft: true,
			state: String::from("OPEN"),
			url: String::from("https://github.com/helixbox/maestro/pull/44"),
		})]);
		let local_repo_inspector = FakeLocalRepoInspector::new(vec![Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/44",
				"summary": "Ready for review."
			}),
		);

		assert!(!response.success);
		assert!(tracker.state_updates.borrow().is_empty());
	}

	#[test]
	fn rejects_review_handoff_for_stale_pr_head() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![Ok(PullRequestDetails {
			head_ref_name: String::from("x/maestro-pub-618"),
			head_ref_oid: String::from("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
			head_repository_name: String::from("maestro"),
			head_repository_owner: String::from("helixbox"),
			is_draft: false,
			state: String::from("OPEN"),
			url: String::from("https://github.com/helixbox/maestro/pull/45"),
		})]);
		let local_repo_inspector = FakeLocalRepoInspector::new(vec![Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/helixbox/maestro/pull/45",
				"summary": "Ready for review."
			}),
		);

		assert!(!response.success);
		assert!(bridge.apply_review_handoff().is_err());
	}

	#[test]
	fn rejects_review_handoff_for_another_repository() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(vec![Ok(PullRequestDetails {
			head_ref_name: String::from("x/maestro-pub-618"),
			head_ref_oid: String::from("08a20f7dfb9526e7421a5f095b1c6adec84e52d6"),
			head_repository_name: String::from("maestro-fork"),
			head_repository_owner: String::from("someone-else"),
			is_draft: false,
			state: String::from("OPEN"),
			url: String::from("https://github.com/someone-else/maestro-fork/pull/46"),
		})]);
		let local_repo_inspector = FakeLocalRepoInspector::new(vec![Ok(sample_local_repo())]);
		let bridge = TrackerToolBridge::with_review_handoff_for_test(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
			&local_repo_inspector,
		);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			ISSUE_REVIEW_HANDOFF_TOOL_NAME,
			serde_json::json!({
				"pr_url": "https://github.com/someone-else/maestro-fork/pull/46",
				"summary": "Ready for review."
			}),
		);

		assert!(!response.success);
		assert!(bridge.apply_review_handoff().is_err());
	}

	#[test]
	fn parses_credentialed_https_github_remote() {
		let repository = super::parse_github_repository_identity(
			"https://x-access-token@github.com/helixbox/maestro.git",
		)
		.expect("credentialed GitHub remote should parse");

		assert_eq!(
			repository,
			super::RepositoryIdentity {
				owner: String::from("helixbox"),
				name: String::from("maestro"),
			}
		);
	}

	#[test]
	fn publishes_protocol_safe_tool_names() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let inspector = FakePullRequestInspector::new(Vec::new());
		let bridge = TrackerToolBridge::with_review_handoff(
			&tracker,
			&issue,
			&workflow,
			sample_review_context(),
			&inspector,
		);
		let tool_specs = DynamicToolHandler::tool_specs(&bridge);

		assert!(!tool_specs.is_empty());
		assert!(tool_specs.into_iter().all(|tool| {
			tool.name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
		}));
	}
}
