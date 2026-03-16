use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::{
	prelude::{Result, eyre},
	tracker::{
		IssueTracker, TrackerIssue, TrackerIssueBlocker, TrackerLabel, TrackerProject,
		TrackerState, TrackerTeam,
	},
};

const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";
const PROJECT_QUERY: &str = r#"
query ProjectBySlug($projectSlug: String!) {
  projects(filter: { slugId: { eq: $projectSlug } }, first: 1) {
    nodes {
      id
      name
      slugId
    }
  }
}
"#;
const ISSUES_QUERY: &str = r#"
query IssuesForProject($projectSlug: String!, $after: String) {
  issues(filter: { project: { slugId: { eq: $projectSlug } } }, first: 50, after: $after) {
    nodes {
      id
      identifier
      title
      description
      priority
      createdAt
      state {
        id
        name
      }
      team {
        id
        name
        states(first: 50) {
          nodes {
            id
            name
          }
        }
        labels(first: 100) {
          nodes {
            id
            name
          }
        }
      }
      labels(first: 50) {
        nodes {
          id
          name
        }
      }
      inverseRelations(first: 50) {
        nodes {
          type
          issue {
            id
            identifier
            state {
              id
              name
            }
          }
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;
const ISSUES_BY_IDS_QUERY: &str = r#"
query IssuesByIds($issueIds: [ID!], $after: String) {
  issues(filter: { id: { in: $issueIds } }, first: 50, after: $after) {
    nodes {
      id
      identifier
      title
      description
      priority
      createdAt
      state {
        id
        name
      }
      team {
        id
        name
        states(first: 50) {
          nodes {
            id
            name
          }
        }
        labels(first: 100) {
          nodes {
            id
            name
          }
        }
      }
      labels(first: 50) {
        nodes {
          id
          name
        }
      }
      inverseRelations(first: 50) {
        nodes {
          type
          issue {
            id
            identifier
            state {
              id
              name
            }
          }
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;
const ISSUE_BLOCKERS_QUERY: &str = r#"
query IssueBlockers($issueId: String!, $after: String) {
  issues(filter: { id: { eq: $issueId } }, first: 1) {
    nodes {
      inverseRelations(first: 50, after: $after) {
        nodes {
          type
          issue {
            id
            identifier
            state {
              id
              name
            }
          }
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
  }
}
"#;
const ISSUE_UPDATE_MUTATION: &str = r#"
mutation UpdateIssue($id: String!, $input: IssueUpdateInput!) {
  issueUpdate(id: $id, input: $input) {
    success
  }
}
"#;
const COMMENT_CREATE_MUTATION: &str = r#"
mutation CreateComment($input: CommentCreateInput!) {
  commentCreate(input: $input) {
    success
  }
}
"#;

pub(crate) struct LinearClient {
	api_token: String,
	http: Client,
}
impl LinearClient {
	pub(crate) fn new(api_token: String) -> Result<Self> {
		Ok(Self { api_token, http: Client::builder().build()? })
	}

	fn post<V, T>(&self, query: &str, variables: &V) -> Result<T>
	where
		V: Serialize,
		T: for<'de> Deserialize<'de>,
	{
		let response = self
			.http
			.post(LINEAR_GRAPHQL_URL)
			.header("Authorization", &self.api_token)
			.json(&GraphqlRequest { query, variables })
			.send()?
			.error_for_status()?;
		let payload = response.json::<GraphqlResponse<T>>()?;

		if let Some(errors) = payload.errors {
			let messages =
				errors.into_iter().map(|error| error.message).collect::<Vec<_>>().join("; ");

			eyre::bail!("Linear GraphQL request failed: {messages}");
		}

		payload.data.ok_or_else(|| eyre::eyre!("Linear GraphQL response did not include data."))
	}

	fn collect_issue_pages<V, F>(
		&self,
		query: &str,
		mut make_variables: F,
	) -> Result<Vec<TrackerIssue>>
	where
		V: Serialize,
		F: FnMut(Option<String>) -> V,
	{
		let mut after = None;
		let mut issues = Vec::new();

		loop {
			let data =
				self.post::<_, IssueConnectionData>(query, &make_variables(after.clone()))?;
			let connection = data.issues;

			for issue in connection.nodes {
				let blockers = self.resolve_issue_blockers(&issue)?;

				issues.push(map_issue(issue, blockers));
			}

			if !connection.page_info.has_next_page {
				break;
			}

			after = Some(require_end_cursor(
				connection.page_info,
				"Linear issue pagination reported `hasNextPage = true` without an `endCursor`.",
			)?);
		}

		Ok(issues)
	}

	fn resolve_issue_blockers(&self, issue: &LinearIssue) -> Result<Vec<TrackerIssueBlocker>> {
		let mut blockers = map_blockers(&issue.inverse_relations.nodes);

		if issue.state.name != "Todo" || !issue.inverse_relations.page_info.has_next_page {
			return Ok(blockers);
		}

		let mut after = Some(require_end_cursor(
			issue.inverse_relations.page_info.clone(),
			"Linear blocker pagination reported `hasNextPage = true` without an `endCursor`.",
		)?);

		while let Some(cursor) = after {
			let data = self.post::<_, IssueBlockersData>(
				ISSUE_BLOCKERS_QUERY,
				&IssueBlockersVariables { issue_id: issue.id.clone(), after: Some(cursor) },
			)?;
			let Some(issue_page) = data.issues.nodes.into_iter().next() else {
				eyre::bail!(
					"Linear blocker pagination did not return the requested issue `{}`.",
					issue.id
				);
			};
			let blocker_page = issue_page.inverse_relations;

			blockers.extend(map_blockers(&blocker_page.nodes));

			after = if blocker_page.page_info.has_next_page {
				Some(require_end_cursor(
					blocker_page.page_info,
					"Linear blocker pagination reported `hasNextPage = true` without an `endCursor`.",
				)?)
			} else {
				None
			};
		}

		Ok(blockers)
	}
}
impl IssueTracker for LinearClient {
	fn list_project_issues(&self, project_slug: &str) -> Result<Vec<TrackerIssue>> {
		self.collect_issue_pages(ISSUES_QUERY, |after| IssuesForProjectVariables {
			project_slug: project_slug.to_owned(),
			after,
		})
	}

	fn get_project_by_slug(&self, project_slug: &str) -> Result<Option<TrackerProject>> {
		let data = self.post::<_, ProjectBySlugData>(
			PROJECT_QUERY,
			&ProjectBySlugVariables { project_slug: project_slug.to_owned() },
		)?;

		Ok(data.projects.nodes.into_iter().next().map(|project| TrackerProject {
			id: project.id,
			name: project.name,
			slug: project.slug,
		}))
	}

	fn refresh_issues(&self, issue_ids: &[String]) -> Result<Vec<TrackerIssue>> {
		if issue_ids.is_empty() {
			return Ok(Vec::new());
		}

		self.collect_issue_pages(ISSUES_BY_IDS_QUERY, |after| IssuesByIdsVariables {
			issue_ids: issue_ids.to_vec(),
			after,
		})
	}

	fn update_issue_state(&self, issue_id: &str, state_id: &str) -> Result<()> {
		let data = self.post::<_, IssueUpdateData>(
			ISSUE_UPDATE_MUTATION,
			&IssueUpdateVariables {
				id: issue_id,
				input: IssueUpdateInput { state_id: Some(state_id.to_owned()), label_ids: None },
			},
		)?;

		if !data.issue_update.success {
			eyre::bail!("Linear did not confirm the issue state update.");
		}

		Ok(())
	}

	fn update_issue_labels(&self, issue_id: &str, label_ids: &[String]) -> Result<()> {
		let data = self.post::<_, IssueUpdateData>(
			ISSUE_UPDATE_MUTATION,
			&IssueUpdateVariables {
				id: issue_id,
				input: IssueUpdateInput { state_id: None, label_ids: Some(label_ids.to_vec()) },
			},
		)?;

		if !data.issue_update.success {
			eyre::bail!("Linear did not confirm the issue label update.");
		}

		Ok(())
	}

	fn create_comment(&self, issue_id: &str, body: &str) -> Result<()> {
		let data = self.post::<_, CommentCreateData>(
			COMMENT_CREATE_MUTATION,
			&CommentCreateVariables {
				input: CommentCreateInput { body: body.to_owned(), issue_id: issue_id.to_owned() },
			},
		)?;

		if !data.comment_create.success {
			eyre::bail!("Linear did not confirm the comment creation.");
		}

		Ok(())
	}
}

#[derive(Serialize)]
struct GraphqlRequest<'a, V> {
	query: &'a str,
	variables: V,
}

#[derive(Deserialize)]
struct GraphqlResponse<T> {
	data: Option<T>,
	errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
struct GraphqlError {
	message: String,
}

#[derive(Serialize)]
struct ProjectBySlugVariables {
	#[serde(rename = "projectSlug")]
	project_slug: String,
}

#[derive(Deserialize)]
struct ProjectBySlugData {
	projects: ProjectConnection,
}

#[derive(Deserialize)]
struct ProjectConnection {
	nodes: Vec<LinearProject>,
}

#[derive(Deserialize)]
struct LinearProject {
	id: String,
	name: String,
	#[serde(rename = "slugId")]
	slug: String,
}

#[derive(Serialize)]
struct IssuesForProjectVariables {
	#[serde(rename = "projectSlug")]
	project_slug: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	after: Option<String>,
}

#[derive(Serialize)]
struct IssuesByIdsVariables {
	#[serde(rename = "issueIds")]
	issue_ids: Vec<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	after: Option<String>,
}

#[derive(Serialize)]
struct IssueBlockersVariables {
	#[serde(rename = "issueId")]
	issue_id: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	after: Option<String>,
}

#[derive(Deserialize)]
struct IssueConnectionData {
	issues: IssueConnection,
}

#[derive(Deserialize)]
struct IssueBlockersData {
	issues: IssueBlockerConnection,
}

#[derive(Deserialize)]
struct IssueConnection {
	nodes: Vec<LinearIssue>,
	#[serde(rename = "pageInfo")]
	page_info: PageInfo,
}

#[derive(Deserialize)]
struct IssueBlockerConnection {
	nodes: Vec<LinearIssueBlockerPage>,
}

#[derive(Deserialize)]
struct LinearIssueBlockerPage {
	#[serde(rename = "inverseRelations")]
	inverse_relations: IssueRelationConnection,
}

#[derive(Clone, Deserialize)]
struct PageInfo {
	#[serde(rename = "hasNextPage")]
	has_next_page: bool,
	#[serde(rename = "endCursor")]
	end_cursor: Option<String>,
}

#[derive(Deserialize)]
struct LinearIssue {
	id: String,
	identifier: String,
	title: String,
	description: Option<String>,
	priority: Option<i64>,
	#[serde(rename = "createdAt")]
	created_at: String,
	state: LinearState,
	team: LinearTeam,
	labels: LabelConnection,
	#[serde(rename = "inverseRelations")]
	inverse_relations: IssueRelationConnection,
}

#[derive(Deserialize)]
struct LinearTeam {
	id: String,
	name: String,
	states: StateConnection,
	labels: LabelConnection,
}

#[derive(Deserialize)]
struct StateConnection {
	nodes: Vec<LinearState>,
}

#[derive(Deserialize)]
struct LabelConnection {
	nodes: Vec<LinearLabel>,
}

#[derive(Deserialize)]
struct IssueRelationConnection {
	nodes: Vec<LinearIssueRelation>,
	#[serde(rename = "pageInfo")]
	page_info: PageInfo,
}

#[derive(Deserialize)]
struct LinearIssueRelation {
	#[serde(rename = "type")]
	relation_type: String,
	issue: LinearRelatedIssue,
}

#[derive(Deserialize)]
struct LinearRelatedIssue {
	id: String,
	identifier: String,
	state: LinearState,
}

#[derive(Deserialize)]
struct LinearState {
	id: String,
	name: String,
}

#[derive(Deserialize)]
struct LinearLabel {
	id: String,
	name: String,
}

#[derive(Serialize)]
struct IssueUpdateVariables<'a> {
	id: &'a str,
	input: IssueUpdateInput,
}

#[derive(Serialize)]
struct IssueUpdateInput {
	#[serde(rename = "stateId", skip_serializing_if = "Option::is_none")]
	state_id: Option<String>,
	#[serde(rename = "labelIds", skip_serializing_if = "Option::is_none")]
	label_ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct IssueUpdateData {
	#[serde(rename = "issueUpdate")]
	issue_update: MutationSuccess,
}

#[derive(Deserialize)]
struct MutationSuccess {
	success: bool,
}

#[derive(Serialize)]
struct CommentCreateVariables {
	input: CommentCreateInput,
}

#[derive(Serialize)]
struct CommentCreateInput {
	body: String,
	#[serde(rename = "issueId")]
	issue_id: String,
}

#[derive(Deserialize)]
struct CommentCreateData {
	#[serde(rename = "commentCreate")]
	comment_create: MutationSuccess,
}

fn require_end_cursor(page_info: PageInfo, message: &str) -> Result<String> {
	page_info.end_cursor.ok_or_else(|| eyre::eyre!(message.to_owned()))
}

fn map_blockers(relations: &[LinearIssueRelation]) -> Vec<TrackerIssueBlocker> {
	relations
		.iter()
		.filter(|relation| relation.relation_type == "blocks")
		.map(|relation| TrackerIssueBlocker {
			id: relation.issue.id.clone(),
			identifier: relation.issue.identifier.clone(),
			state: TrackerState {
				id: relation.issue.state.id.clone(),
				name: relation.issue.state.name.clone(),
			},
		})
		.collect()
}

fn map_issue(issue: LinearIssue, blockers: Vec<TrackerIssueBlocker>) -> TrackerIssue {
	TrackerIssue {
		id: issue.id,
		identifier: issue.identifier,
		title: issue.title,
		description: issue.description.unwrap_or_default(),
		priority: issue.priority,
		created_at: issue.created_at,
		state: TrackerState { id: issue.state.id, name: issue.state.name },
		team: TrackerTeam {
			id: issue.team.id,
			name: issue.team.name,
			states: issue
				.team
				.states
				.nodes
				.into_iter()
				.map(|state| TrackerState { id: state.id, name: state.name })
				.collect(),
			labels: issue
				.team
				.labels
				.nodes
				.into_iter()
				.map(|label| TrackerLabel { id: label.id, name: label.name })
				.collect(),
		},
		labels: issue
			.labels
			.nodes
			.into_iter()
			.map(|label| TrackerLabel { id: label.id, name: label.name })
			.collect(),
		blockers,
	}
}

#[cfg(test)]
mod tests {
	use crate::tracker::linear::{
		IssueRelationConnection, LabelConnection, LinearIssue, LinearIssueRelation, LinearLabel,
		LinearRelatedIssue, LinearState, LinearTeam, PageInfo, StateConnection,
	};

	#[test]
	fn map_issue_preserves_priority_and_created_at() {
		let issue = LinearIssue {
			id: String::from("issue-1"),
			identifier: String::from("PUB-101"),
			title: String::from("Implement ordering"),
			description: Some(String::from("Body")),
			priority: Some(2),
			created_at: String::from("2026-03-13T04:16:17.133Z"),
			state: LinearState { id: String::from("state-todo"), name: String::from("Todo") },
			team: LinearTeam {
				id: String::from("team-1"),
				name: String::from("Pubfi"),
				states: StateConnection {
					nodes: vec![LinearState {
						id: String::from("state-todo"),
						name: String::from("Todo"),
					}],
				},
				labels: LabelConnection {
					nodes: vec![LinearLabel {
						id: String::from("label-needs"),
						name: String::from("maestro:needs-attention"),
					}],
				},
			},
			labels: LabelConnection {
				nodes: vec![LinearLabel {
					id: String::from("label-manual"),
					name: String::from("maestro:manual-only"),
				}],
			},
			inverse_relations: IssueRelationConnection {
				nodes: vec![LinearIssueRelation {
					relation_type: String::from("blocks"),
					issue: LinearRelatedIssue {
						id: String::from("issue-2"),
						identifier: String::from("PUB-102"),
						state: LinearState {
							id: String::from("state-progress"),
							name: String::from("In Progress"),
						},
					},
				}],
				page_info: PageInfo { has_next_page: false, end_cursor: None },
			},
		};
		let blockers = super::map_blockers(&issue.inverse_relations.nodes);
		let mapped = super::map_issue(issue, blockers);

		assert_eq!(mapped.priority, Some(2));
		assert_eq!(mapped.created_at, "2026-03-13T04:16:17.133Z");
		assert_eq!(mapped.blockers.len(), 1);
		assert_eq!(mapped.blockers[0].identifier, "PUB-102");
		assert_eq!(mapped.blockers[0].state.name, "In Progress");
	}

	#[test]
	fn map_blockers_filters_non_blocking_relations() {
		let blockers = super::map_blockers(&[
			LinearIssueRelation {
				relation_type: String::from("blocks"),
				issue: LinearRelatedIssue {
					id: String::from("issue-2"),
					identifier: String::from("PUB-102"),
					state: LinearState {
						id: String::from("state-progress"),
						name: String::from("In Progress"),
					},
				},
			},
			LinearIssueRelation {
				relation_type: String::from("related"),
				issue: LinearRelatedIssue {
					id: String::from("issue-3"),
					identifier: String::from("PUB-103"),
					state: LinearState {
						id: String::from("state-done"),
						name: String::from("Done"),
					},
				},
			},
		]);

		assert_eq!(blockers.len(), 1);
		assert_eq!(blockers[0].identifier, "PUB-102");
	}
}
