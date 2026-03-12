pub(crate) mod linear;

use crate::prelude::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackerIssue {
	pub(crate) id: String,
	pub(crate) identifier: String,
	pub(crate) title: String,
	pub(crate) description: String,
	pub(crate) state: TrackerState,
	pub(crate) team: TrackerTeam,
	pub(crate) labels: Vec<TrackerLabel>,
}
impl TrackerIssue {
	pub(crate) fn has_label(&self, label_name: &str) -> bool {
		self.labels.iter().any(|label| label.name == label_name)
	}

	pub(crate) fn state_id_for_name(&self, state_name: &str) -> Option<&str> {
		self.team
			.states
			.iter()
			.find(|state| state.name == state_name)
			.map(|state| state.id.as_str())
	}

	pub(crate) fn label_id_for_name(&self, label_name: &str) -> Option<&str> {
		self.team
			.labels
			.iter()
			.find(|label| label.name == label_name)
			.map(|label| label.id.as_str())
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackerState {
	pub(crate) id: String,
	pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackerLabel {
	pub(crate) id: String,
	pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackerTeam {
	pub(crate) id: String,
	pub(crate) name: String,
	pub(crate) states: Vec<TrackerState>,
	pub(crate) labels: Vec<TrackerLabel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackerProject {
	pub(crate) id: String,
	pub(crate) name: String,
	pub(crate) slug: String,
}

pub(crate) trait IssueTracker {
	fn list_project_issues(&self, project_slug: &str) -> Result<Vec<TrackerIssue>>;
	fn get_project_by_slug(&self, project_slug: &str) -> Result<Option<TrackerProject>>;
	fn refresh_issues(&self, issue_ids: &[String]) -> Result<Vec<TrackerIssue>>;
	fn update_issue_state(&self, issue_id: &str, state_id: &str) -> Result<()>;
	fn update_issue_labels(&self, issue_id: &str, label_ids: &[String]) -> Result<()>;
	fn create_comment(&self, issue_id: &str, body: &str) -> Result<()>;
}
