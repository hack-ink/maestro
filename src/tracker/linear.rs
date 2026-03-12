use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::{
	prelude::{Result, eyre},
	tracker::{
		IssueTracker, TrackerIssue, TrackerLabel, TrackerProject, TrackerState, TrackerTeam,
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
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;
const ISSUES_BY_IDS_QUERY: &str = r#"
query IssuesByIds($issueIds: [String!], $after: String) {
  issues(filter: { id: { in: $issueIds } }, first: 50, after: $after) {
    nodes {
      id
      identifier
      title
      description
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
    }
    pageInfo {
      hasNextPage
      endCursor
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
			.bearer_auth(&self.api_token)
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

			issues.extend(connection.nodes.into_iter().map(map_issue));

			if !connection.page_info.has_next_page {
				break;
			}

			after = connection.page_info.end_cursor;
			if after.is_none() {
				eyre::bail!(
					"Linear pagination reported `hasNextPage = true` without an `endCursor`."
				);
			}
		}

		Ok(issues)
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

#[derive(Deserialize)]
struct IssueConnectionData {
	issues: IssueConnection,
}

#[derive(Deserialize)]
struct IssueConnection {
	nodes: Vec<LinearIssue>,
	#[serde(rename = "pageInfo")]
	page_info: PageInfo,
}

#[derive(Deserialize)]
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
	state: LinearState,
	team: LinearTeam,
	labels: LabelConnection,
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

fn map_issue(issue: LinearIssue) -> TrackerIssue {
	TrackerIssue {
		id: issue.id,
		identifier: issue.identifier,
		title: issue.title,
		description: issue.description.unwrap_or_default(),
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
	}
}
