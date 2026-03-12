use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
	tracker::{IssueTracker, TrackerIssue},
	workflow::WorkflowDocument,
};

pub(crate) trait DynamicToolHandler {
	fn tool_specs(&self) -> Vec<DynamicToolSpec>;
	fn handle_call(&self, tool_name: &str, arguments: Value) -> DynamicToolCallResponse;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DynamicToolSpec {
	pub(crate) description: String,
	#[serde(rename = "inputSchema")]
	pub(crate) input_schema: Value,
	pub(crate) name: String,
}

#[derive(Clone, Copy)]
pub(crate) struct TrackerToolBridge<'a> {
	tracker: &'a dyn IssueTracker,
	issue: &'a TrackerIssue,
	workflow: &'a WorkflowDocument,
}
impl<'a> TrackerToolBridge<'a> {
	pub(crate) fn new(
		tracker: &'a dyn IssueTracker,
		issue: &'a TrackerIssue,
		workflow: &'a WorkflowDocument,
	) -> Self {
		Self { tracker, issue, workflow }
	}

	fn build_tool_specs(&self) -> Vec<DynamicToolSpec> {
		vec![
			DynamicToolSpec {
				name: String::from("issue.transition"),
				description: String::from(
					"Move the currently leased issue to another allowed workflow state.",
				),
				input_schema: json!({
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
				name: String::from("issue.comment"),
				description: String::from("Add a comment to the currently leased issue."),
				input_schema: json!({
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
			DynamicToolSpec {
				name: String::from("issue.label.add"),
				description: String::from(
					"Add an allowed workflow label to the currently leased issue.",
				),
				input_schema: json!({
					"type": "object",
					"properties": {
						"issue_id": { "type": "string" },
						"issue_identifier": { "type": "string" },
						"label": { "type": "string" }
					},
					"required": ["label"],
					"additionalProperties": false
				}),
			},
		]
	}

	fn handle_call_inner(&self, tool_name: &str, arguments: Value) -> DynamicToolCallResponse {
		match tool_name {
			"issue.transition" => self.handle_transition(arguments),
			"issue.comment" => self.handle_comment(arguments),
			"issue.label.add" => self.handle_add_label(arguments),
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
			Ok(()) => DynamicToolCallResponse::success(format!(
				"Issue `{}` moved to `{}`.",
				self.issue.identifier, parsed.state
			)),
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

		match self.tracker.create_comment(&self.issue.id, &parsed.body) {
			Ok(()) => DynamicToolCallResponse::success(format!(
				"Comment added to issue `{}`.",
				self.issue.identifier
			)),
			Err(error) => DynamicToolCallResponse::failure(format!(
				"Failed to add a comment to issue `{}`: {error}",
				self.issue.identifier
			)),
		}
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

		let Some(label_id) = self.issue.label_id_for_name(&parsed.label) else {
			return DynamicToolCallResponse::failure(format!(
				"Label `{}` does not exist on issue `{}`.",
				parsed.label, self.issue.identifier
			));
		};
		let mut label_ids =
			self.issue.labels.iter().map(|label| label.id.clone()).collect::<Vec<_>>();

		if label_ids.iter().any(|existing| existing == label_id) {
			return DynamicToolCallResponse::success(format!(
				"Issue `{}` already has label `{}`.",
				self.issue.identifier, parsed.label
			));
		}

		label_ids.push(label_id.to_owned());

		match self.tracker.update_issue_labels(&self.issue.id, &label_ids) {
			Ok(()) => DynamicToolCallResponse::success(format!(
				"Label `{}` added to issue `{}`.",
				parsed.label, self.issue.identifier
			)),
			Err(error) => DynamicToolCallResponse::failure(format!(
				"Failed to add label `{}` to issue `{}`: {error}",
				parsed.label, self.issue.identifier
			)),
		}
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
		let mut states = tracker.startable_states().iter().map(String::as_str).collect::<Vec<_>>();

		for state in [tracker.in_progress_state(), tracker.success_state(), tracker.failure_state()]
		{
			if !states.iter().any(|existing| existing == &state) {
				states.push(state);
			}
		}

		states
	}
}
impl DynamicToolHandler for TrackerToolBridge<'_> {
	fn tool_specs(&self) -> Vec<DynamicToolSpec> {
		self.build_tool_specs()
	}

	fn handle_call(&self, tool_name: &str, arguments: Value) -> DynamicToolCallResponse {
		self.handle_call_inner(tool_name, arguments)
	}
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

#[derive(Debug, Deserialize)]
struct ScopeArgs {
	issue_id: Option<String>,

	issue_identifier: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TransitionArgs {
	#[serde(flatten)]
	scope: ScopeArgs,
	#[serde(alias = "state_name")]
	state: String,
}

#[derive(Debug, Deserialize)]
struct CommentArgs {
	#[serde(flatten)]
	scope: ScopeArgs,
	body: String,
}

#[derive(Debug, Deserialize)]
struct LabelArgs {
	#[serde(flatten)]
	scope: ScopeArgs,
	#[serde(alias = "label_name")]
	label: String,
}

#[cfg(test)]
mod tests {
	use std::cell::RefCell;

	use crate::{
		agent::tracker_tool_bridge::{DynamicToolHandler, TrackerToolBridge},
		prelude::Result,
		tracker::{
			IssueTracker, TrackerIssue, TrackerLabel, TrackerProject, TrackerState, TrackerTeam,
		},
		workflow::WorkflowDocument,
	};

	struct FakeTracker {
		state_updates: RefCell<Vec<String>>,
		label_updates: RefCell<Vec<Vec<String>>>,
		comments: RefCell<Vec<String>>,
	}
	impl FakeTracker {
		fn new() -> Self {
			Self {
				state_updates: RefCell::new(Vec::new()),
				label_updates: RefCell::new(Vec::new()),
				comments: RefCell::new(Vec::new()),
			}
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
			Ok(Vec::new())
		}

		fn update_issue_state(&self, _issue_id: &str, state_id: &str) -> Result<()> {
			self.state_updates.borrow_mut().push(state_id.to_owned());

			Ok(())
		}

		fn update_issue_labels(&self, _issue_id: &str, label_ids: &[String]) -> Result<()> {
			self.label_updates.borrow_mut().push(label_ids.to_vec());

			Ok(())
		}

		fn create_comment(&self, _issue_id: &str, body: &str) -> Result<()> {
			self.comments.borrow_mut().push(body.to_owned());

			Ok(())
		}
	}

	fn sample_issue() -> TrackerIssue {
		TrackerIssue {
			id: String::from("issue-1"),
			identifier: String::from("MAE-1"),
			title: String::from("Sample"),
			description: String::from("Body"),
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
		}
	}

	fn sample_workflow() -> WorkflowDocument {
		WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "maestro"
startable_states = ["Todo"]
in_progress_state = "In Progress"
success_state = "In Review"
failure_state = "Todo"
opt_out_label = "maestro:manual-only"
needs_attention_label = "maestro:needs-attention"
+++

Use the tracker tools.
"#,
		)
		.expect("workflow should parse")
	}

	#[test]
	fn transitions_current_issue_with_allowed_state() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			"issue.transition",
			serde_json::json!({ "issue_identifier": "MAE-1", "state": "In Progress" }),
		);

		assert!(response.success);
		assert_eq!(tracker.state_updates.borrow().as_slice(), ["state-progress"]);
	}

	#[test]
	fn rejects_tool_calls_for_another_issue() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			"issue.comment",
			serde_json::json!({ "issue_identifier": "MAE-999", "body": "hello" }),
		);

		assert!(!response.success);
		assert!(tracker.comments.borrow().is_empty());
	}

	#[test]
	fn adds_allowed_workflow_label() {
		let tracker = FakeTracker::new();
		let issue = sample_issue();
		let workflow = sample_workflow();
		let bridge = TrackerToolBridge::new(&tracker, &issue, &workflow);
		let response = DynamicToolHandler::handle_call(
			&bridge,
			"issue.label.add",
			serde_json::json!({ "label": "maestro:needs-attention" }),
		);

		assert!(response.success);
		assert_eq!(tracker.label_updates.borrow().as_slice(), [vec![String::from("label-needs")]]);
	}
}
