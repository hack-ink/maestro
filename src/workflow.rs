//! Downstream `WORKFLOW.md` parsing and validation.

use std::{collections::HashMap, fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::prelude::{Result, eyre};

const FRONTMATTER_DELIMITER: &str = "+++";

/// Parsed downstream workflow document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowDocument {
	frontmatter: WorkflowFrontmatter,
	body: String,
}
impl WorkflowDocument {
	/// Parse a workflow document from Markdown text.
	pub fn parse_markdown(input: &str) -> Result<Self> {
		let (frontmatter_input, body) = split_frontmatter(input)?;
		let frontmatter = toml::from_str::<WorkflowFrontmatter>(&frontmatter_input)?;

		frontmatter.validate()?;

		Ok(Self { frontmatter, body })
	}

	/// Load a workflow document from the repository root.
	pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
		let input = fs::read_to_string(path)?;

		Self::parse_markdown(&input)
	}

	/// Machine-readable frontmatter for orchestration behavior.
	pub fn frontmatter(&self) -> &WorkflowFrontmatter {
		&self.frontmatter
	}

	/// Human-readable Markdown policy body.
	pub fn body(&self) -> &str {
		&self.body
	}

	/// Render the workflow back to Markdown for process-to-process handoff.
	pub fn to_markdown(&self) -> Result<String> {
		let frontmatter = toml::to_string(&self.frontmatter)?;
		let mut markdown = format!("{FRONTMATTER_DELIMITER}\n{frontmatter}{FRONTMATTER_DELIMITER}");

		if !self.body.is_empty() {
			markdown.push_str("\n\n");
			markdown.push_str(&self.body);
		}

		Ok(markdown)
	}
}

/// Typed TOML frontmatter for a downstream workflow document.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WorkflowFrontmatter {
	version: u8,
	tracker: WorkflowTracker,
	#[serde(default)]
	agent: WorkflowAgent,
	#[serde(default)]
	execution: WorkflowExecution,
	#[serde(default)]
	context: WorkflowContext,
}
impl WorkflowFrontmatter {
	/// Contract version.
	pub fn version(&self) -> u8 {
		self.version
	}

	/// Tracker policy for this repository.
	pub fn tracker(&self) -> &WorkflowTracker {
		&self.tracker
	}

	/// Agent defaults for this repository.
	pub fn agent(&self) -> &WorkflowAgent {
		&self.agent
	}

	/// Execution policy for this repository.
	pub fn execution(&self) -> &WorkflowExecution {
		&self.execution
	}

	/// Extra early-load context paths for this repository.
	pub fn context(&self) -> &WorkflowContext {
		&self.context
	}

	fn validate(&self) -> Result<()> {
		if self.version != 1 {
			eyre::bail!("Unsupported WORKFLOW.md version: {}", self.version);
		}
		if self.tracker.startable_states.is_empty() {
			eyre::bail!("`tracker.startable_states` must not be empty.");
		}
		if self.tracker.project_slug.trim().is_empty() {
			eyre::bail!("`tracker.project_slug` must not be empty.");
		}
		if self.execution.max_attempts == 0 {
			eyre::bail!("`execution.max_attempts` must be greater than zero.");
		}
		if self.execution.max_turns == 0 {
			eyre::bail!("`execution.max_turns` must be greater than zero.");
		}
		if self.execution.max_retry_backoff_ms == 0 {
			eyre::bail!("`execution.max_retry_backoff_ms` must be greater than zero.");
		}

		if let Some(completed_state) = self.tracker.completed_state()
			&& !self.tracker.terminal_states.iter().any(|state| state == completed_state)
		{
			eyre::bail!("`tracker.completed_state` must be one of `tracker.terminal_states`.");
		}

		self.execution.validate()?;

		Ok(())
	}
}

/// Tracker-facing repository policy.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTracker {
	provider: TrackerProvider,
	project_slug: String,
	#[serde(default = "default_startable_states")]
	startable_states: Vec<String>,
	#[serde(default = "default_terminal_states")]
	terminal_states: Vec<String>,
	#[serde(default = "default_in_progress_state")]
	in_progress_state: String,
	#[serde(default = "default_success_state")]
	success_state: String,
	completed_state: Option<String>,
	#[serde(default = "default_failure_state")]
	failure_state: String,
	#[serde(default = "default_opt_out_label")]
	opt_out_label: String,
	#[serde(default = "default_needs_attention_label")]
	needs_attention_label: String,
}
impl WorkflowTracker {
	/// Tracker provider for this repository.
	pub fn provider(&self) -> TrackerProvider {
		self.provider
	}

	/// Stable tracker project slug.
	pub fn project_slug(&self) -> &str {
		&self.project_slug
	}

	/// States that are eligible for automatic execution.
	pub fn startable_states(&self) -> &[String] {
		&self.startable_states
	}

	/// States that are considered terminal for automatic execution.
	pub fn terminal_states(&self) -> &[String] {
		&self.terminal_states
	}

	/// State used when `maestro` starts work on an issue.
	pub fn in_progress_state(&self) -> &str {
		&self.in_progress_state
	}

	/// State used after a successful run and validation pass.
	pub fn success_state(&self) -> &str {
		&self.success_state
	}

	/// Explicit state used after a successful post-merge closeout.
	pub fn completed_state(&self) -> Option<&str> {
		self.completed_state.as_deref()
	}

	/// Resolved state used after a successful post-merge closeout when workflow
	/// policy can determine one.
	pub fn resolved_completed_state(&self) -> Option<&str> {
		self.resolved_completed_state_candidate()
	}

	/// State used when retries are exhausted.
	pub fn failure_state(&self) -> &str {
		&self.failure_state
	}

	/// Label that disables automation for an issue.
	pub fn opt_out_label(&self) -> &str {
		&self.opt_out_label
	}

	/// Label that marks failed runs needing human attention.
	pub fn needs_attention_label(&self) -> &str {
		&self.needs_attention_label
	}

	fn resolved_completed_state_candidate(&self) -> Option<&str> {
		self.completed_state.as_deref().or_else(|| {
			self.terminal_states.iter().find(|state| state.as_str() == "Done").map(String::as_str)
		})
	}
}

/// Supported tracker providers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerProvider {
	/// Linear issue tracking.
	Linear,
}

/// Repo-local agent defaults.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAgent {
	#[serde(default = "default_transport")]
	transport: String,
	#[serde(default = "default_sandbox")]
	sandbox: String,
	#[serde(default = "default_approval_policy")]
	approval_policy: String,
	personality: Option<String>,
	service_tier: Option<String>,
}
impl WorkflowAgent {
	/// App-server transport.
	pub fn transport(&self) -> &str {
		&self.transport
	}

	/// Sandbox mode for the run.
	pub fn sandbox(&self) -> &str {
		&self.sandbox
	}

	/// Approval policy for the run.
	pub fn approval_policy(&self) -> &str {
		&self.approval_policy
	}

	/// Optional personality override.
	pub fn personality(&self) -> Option<&str> {
		self.personality.as_deref()
	}

	/// Optional service tier override.
	pub fn service_tier(&self) -> Option<&str> {
		self.service_tier.as_deref()
	}
}

impl Default for WorkflowAgent {
	fn default() -> Self {
		Self {
			transport: default_transport(),
			sandbox: default_sandbox(),
			approval_policy: default_approval_policy(),
			personality: None,
			service_tier: None,
		}
	}
}

/// Repo-local execution policy.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WorkflowExecution {
	#[serde(default = "default_max_attempts")]
	max_attempts: u32,
	#[serde(default = "default_max_turns")]
	max_turns: u32,
	#[serde(default = "default_max_retry_backoff_ms")]
	max_retry_backoff_ms: u64,
	max_concurrent_agents: Option<u32>,
	#[serde(default)]
	max_concurrent_agents_by_state: HashMap<String, u32>,
	#[serde(default)]
	validation_commands: Vec<String>,
}
impl WorkflowExecution {
	/// Maximum automatic attempts before human attention is required.
	pub fn max_attempts(&self) -> u32 {
		self.max_attempts
	}

	/// Maximum same-thread turns per bounded run before Maestro yields cleanly.
	pub fn max_turns(&self) -> u32 {
		self.max_turns
	}

	/// Maximum failure-retry backoff in milliseconds.
	pub fn max_retry_backoff_ms(&self) -> u64 {
		self.max_retry_backoff_ms
	}

	/// Validation commands to run before the success writeback is committed.
	pub fn validation_commands(&self) -> &[String] {
		&self.validation_commands
	}

	/// Maximum concurrent agents allowed for this repository.
	pub fn max_concurrent_agents(&self) -> u32 {
		self.max_concurrent_agents.unwrap_or(default_max_concurrent_agents())
	}

	/// Per-state concurrency overrides keyed by tracker state name.
	pub fn max_concurrent_agents_by_state(&self) -> &HashMap<String, u32> {
		&self.max_concurrent_agents_by_state
	}

	/// Maximum concurrent agents allowed for the provided tracker state, when configured.
	pub fn state_concurrency_limit(&self, state_name: &str) -> Option<u32> {
		self.max_concurrent_agents_by_state.get(state_name).copied()
	}

	fn validate(&self) -> Result<()> {
		if let Some(limit) = self.max_concurrent_agents
			&& limit == 0
		{
			eyre::bail!("`execution.max_concurrent_agents` must be greater than zero.");
		}

		let global_limit = self.max_concurrent_agents();

		for (state, limit) in &self.max_concurrent_agents_by_state {
			if *limit == 0 {
				eyre::bail!(
					"`execution.max_concurrent_agents_by_state.{}` must be greater than zero.",
					state
				);
			}
			if *limit > global_limit {
				eyre::bail!(
					"`execution.max_concurrent_agents_by_state.{state}` ({limit}) exceeds `execution.max_concurrent_agents` ({global_limit})."
				);
			}
		}

		Ok(())
	}
}

impl Default for WorkflowExecution {
	fn default() -> Self {
		Self {
			max_attempts: default_max_attempts(),
			max_turns: default_max_turns(),
			max_retry_backoff_ms: default_max_retry_backoff_ms(),
			max_concurrent_agents: None,
			max_concurrent_agents_by_state: HashMap::new(),
			validation_commands: Vec::new(),
		}
	}
}

/// Repo-local early-load context.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WorkflowContext {
	#[serde(default = "default_read_first")]
	read_first: Vec<String>,
}
impl WorkflowContext {
	/// Repository-relative files to load before the broader prompt body.
	pub fn read_first(&self) -> &[String] {
		&self.read_first
	}
}

impl Default for WorkflowContext {
	fn default() -> Self {
		Self { read_first: default_read_first() }
	}
}

fn split_frontmatter(input: &str) -> Result<(String, String)> {
	let input = input.trim_start_matches(['\u{feff}', '\n', '\r']);
	let mut lines = input.lines();

	if lines.next() != Some(FRONTMATTER_DELIMITER) {
		eyre::bail!("WORKFLOW.md must begin with TOML frontmatter delimited by `+++`.");
	}

	let mut frontmatter_lines = Vec::new();
	let mut body_lines = Vec::new();
	let mut found_end = false;

	for line in lines {
		if !found_end && line == FRONTMATTER_DELIMITER {
			found_end = true;

			continue;
		}
		if found_end {
			body_lines.push(line);
		} else {
			frontmatter_lines.push(line);
		}
	}

	if !found_end {
		eyre::bail!("WORKFLOW.md frontmatter is missing the closing `+++` delimiter.");
	}

	let body = body_lines.join("\n").trim().to_string();

	Ok((frontmatter_lines.join("\n"), body))
}

fn default_startable_states() -> Vec<String> {
	vec![String::from("Todo")]
}

fn default_terminal_states() -> Vec<String> {
	vec![String::from("Done"), String::from("Canceled"), String::from("Duplicate")]
}

fn default_in_progress_state() -> String {
	String::from("In Progress")
}

fn default_success_state() -> String {
	String::from("In Review")
}

fn default_failure_state() -> String {
	String::from("Todo")
}

fn default_opt_out_label() -> String {
	String::from("maestro:manual-only")
}

fn default_needs_attention_label() -> String {
	String::from("maestro:needs-attention")
}

fn default_transport() -> String {
	String::from("stdio://")
}

fn default_sandbox() -> String {
	String::from("workspace-write")
}

fn default_approval_policy() -> String {
	String::from("never")
}

fn default_max_attempts() -> u32 {
	3
}

fn default_max_turns() -> u32 {
	1
}

fn default_max_retry_backoff_ms() -> u64 {
	300_000
}

fn default_max_concurrent_agents() -> u32 {
	1
}

fn default_read_first() -> Vec<String> {
	Vec::new()
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tempfile::NamedTempFile;

	use crate::workflow::{TrackerProvider, WorkflowDocument};

	#[test]
	fn parses_workflow_document() {
		let document = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"

[execution]
max_attempts = 3
max_turns = 4
max_retry_backoff_ms = 300000
max_concurrent_agents = 2
max_concurrent_agents_by_state = { "In Progress" = 1 }
validation_commands = ["cargo make test"]
+++

Start with the repo's documented routing entrypoint when one exists.
Use `cargo make`.
			"#,
		)
		.expect("workflow document should parse");

		assert_eq!(document.frontmatter().version(), 1);
		assert_eq!(document.frontmatter().tracker().provider(), TrackerProvider::Linear);
		assert_eq!(document.frontmatter().tracker().project_slug(), "pubfi");
		assert_eq!(document.frontmatter().execution().max_attempts(), 3);
		assert_eq!(document.frontmatter().execution().max_turns(), 4);
		assert_eq!(document.frontmatter().execution().max_retry_backoff_ms(), 300_000);
		assert_eq!(document.frontmatter().execution().max_concurrent_agents(), 2);
		assert_eq!(
			document.frontmatter().execution().state_concurrency_limit("In Progress"),
			Some(1)
		);
		assert_eq!(
			document.body(),
			"Start with the repo's documented routing entrypoint when one exists.\nUse `cargo make`."
		);
	}

	#[test]
	fn loads_workflow_document_from_path() {
		let file = NamedTempFile::new().expect("temp file should exist");

		fs::write(
			file.path(),
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"
+++

Read the repo policy first.
			"#,
		)
		.expect("workflow document should be written");

		let document =
			WorkflowDocument::from_path(file.path()).expect("workflow should load from path");

		assert_eq!(document.frontmatter().tracker().project_slug(), "pubfi");
	}

	#[test]
	fn parses_explicit_completed_state() {
		let document = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"
terminal_states = ["Released", "Canceled"]
completed_state = "Released"
+++

Read the repo policy first.
			"#,
		)
		.expect("workflow document should parse");

		assert_eq!(document.frontmatter().tracker().completed_state(), Some("Released"));
		assert_eq!(document.frontmatter().tracker().resolved_completed_state(), Some("Released"));
	}

	#[test]
	fn resolves_default_completed_state_from_done_terminal() {
		let document = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"
terminal_states = ["Done", "Canceled", "Duplicate"]
+++

Read the repo policy first.
			"#,
		)
		.expect("workflow document should parse");

		assert_eq!(document.frontmatter().tracker().completed_state(), None);
		assert_eq!(document.frontmatter().tracker().resolved_completed_state(), Some("Done"));
	}

	#[test]
	fn allows_missing_completed_state_when_done_terminal_is_missing() {
		let document = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"
terminal_states = ["Released", "Canceled"]
+++

Read the repo policy first.
			"#,
		)
		.expect("workflow document should parse without a resolved completed_state");

		assert_eq!(document.frontmatter().tracker().completed_state(), None);
		assert_eq!(document.frontmatter().tracker().resolved_completed_state(), None);
	}

	#[test]
	fn rejects_completed_state_outside_terminal_states() {
		let result = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"
terminal_states = ["Released", "Canceled"]
completed_state = "Done"
+++

Read the repo policy first.
			"#,
		);
		let error = result.expect_err("completed_state must belong to terminal_states");

		assert!(
			error
				.to_string()
				.contains("`tracker.completed_state` must be one of `tracker.terminal_states`")
		);
	}

	#[test]
	fn rejects_legacy_project_alias_in_workflow_frontmatter() {
		let result = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project = "pubfi"
+++

Read the repo policy first.
			"#,
		);
		let error = result.expect_err("legacy `project` key should be rejected");

		assert!(error.to_string().contains("unknown field `project`"));
	}

	#[test]
	fn rejects_legacy_project_key_when_project_slug_is_present_in_frontmatter() {
		let result = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"
project = "legacy-pubfi"
+++

Read the repo policy first.
			"#,
		);
		let error =
			result.expect_err("legacy `project` key should be rejected even with `project_slug`");

		assert!(error.to_string().contains("unknown field `project`"));
	}

	#[test]
	fn rejects_legacy_agent_model_field() {
		let result = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"

[agent]
transport = "stdio://"
model = "gpt-5.4"
+++

Read the repo policy first.
			"#,
		);
		let error = result.expect_err("legacy `agent.model` key should be rejected");

		assert!(error.to_string().contains("unknown field `model`"));
	}

	#[test]
	fn rejects_missing_frontmatter() {
		let result = WorkflowDocument::parse_markdown("Read the repo policy first.");

		assert!(result.is_err());
	}

	#[test]
	fn workflow_document_markdown_round_trips() {
		let document = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"

[agent]
transport = "stdio://"
sandbox = "workspace-write"
approval_policy = "never"

[execution]
max_attempts = 5
max_turns = 6
max_retry_backoff_ms = 120000
validation_commands = ["cargo make test"]

[context]
read_first = ["docs/index.md", "README.md"]
+++

Read the repo policy first.
Then validate the lane.
			"#,
		)
		.expect("workflow document should parse");
		let reparsed = WorkflowDocument::parse_markdown(
			&document.to_markdown().expect("workflow markdown should render"),
		)
		.expect("rendered workflow should parse");

		assert_eq!(reparsed, document);
	}

	#[test]
	fn rejects_zero_global_concurrency_limit() {
		let result = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"

[execution]
max_attempts = 3
max_retry_backoff_ms = 300000
max_concurrent_agents = 0
+++
			"#,
		);

		assert!(result.is_err(), "zero global concurrency should be invalid");
	}

	#[test]
	fn rejects_zero_state_concurrency_override() {
		let result = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"

[execution]
max_attempts = 3
max_retry_backoff_ms = 300000
max_concurrent_agents_by_state = { "In Progress" = 0 }
+++
			"#,
		);

		assert!(result.is_err(), "zero state override should be invalid");
	}

	#[test]
	fn rejects_state_override_above_global_limit() {
		let result = WorkflowDocument::parse_markdown(
			r#"
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"

[execution]
max_attempts = 3
max_retry_backoff_ms = 300000
max_concurrent_agents = 1
max_concurrent_agents_by_state = { "In Progress" = 2 }
+++
			"#,
		);

		assert!(result.is_err(), "state override above the global limit should be invalid");
	}
}
