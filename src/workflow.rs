//! Downstream `WORKFLOW.md` parsing and validation.

use std::{fs, path::Path};

use serde::Deserialize;

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
}

/// Typed TOML frontmatter for a downstream workflow document.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
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

		Ok(())
	}
}

/// Tracker-facing repository policy.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct WorkflowTracker {
	provider: TrackerProvider,
	#[serde(alias = "project")]
	project_slug: String,
	#[serde(default = "default_startable_states")]
	startable_states: Vec<String>,
	#[serde(default = "default_terminal_states")]
	terminal_states: Vec<String>,
	#[serde(default = "default_in_progress_state")]
	in_progress_state: String,
	#[serde(default = "default_success_state")]
	success_state: String,
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
}

/// Supported tracker providers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerProvider {
	/// Linear issue tracking.
	Linear,
}

/// Repo-local agent defaults.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct WorkflowAgent {
	#[serde(default = "default_transport")]
	transport: String,
	#[serde(default = "default_sandbox")]
	sandbox: String,
	#[serde(default = "default_approval_policy")]
	approval_policy: String,
	model: Option<String>,
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

	/// Optional model override.
	pub fn model(&self) -> Option<&str> {
		self.model.as_deref()
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
			model: None,
			personality: None,
			service_tier: None,
		}
	}
}

/// Repo-local execution policy.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct WorkflowExecution {
	#[serde(default = "default_max_attempts")]
	max_attempts: u32,
	#[serde(default)]
	validation_commands: Vec<String>,
}
impl WorkflowExecution {
	/// Maximum automatic attempts before human attention is required.
	pub fn max_attempts(&self) -> u32 {
		self.max_attempts
	}

	/// Validation commands to run before the success writeback is committed.
	pub fn validation_commands(&self) -> &[String] {
		&self.validation_commands
	}
}

impl Default for WorkflowExecution {
	fn default() -> Self {
		Self { max_attempts: default_max_attempts(), validation_commands: Vec::new() }
	}
}

/// Repo-local early-load context.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
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

fn default_read_first() -> Vec<String> {
	vec![String::from("AGENTS.md")]
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
validation_commands = ["cargo make test"]
+++

Read `AGENTS.md` first.
Use `cargo make`.
			"#,
		)
		.expect("workflow document should parse");

		assert_eq!(document.frontmatter().version(), 1);
		assert_eq!(document.frontmatter().tracker().provider(), TrackerProvider::Linear);
		assert_eq!(document.frontmatter().tracker().project_slug(), "pubfi");
		assert_eq!(document.frontmatter().execution().max_attempts(), 3);
		assert_eq!(document.body(), "Read `AGENTS.md` first.\nUse `cargo make`.");
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

Read `AGENTS.md` first.
			"#,
		)
		.expect("workflow document should be written");

		let document =
			WorkflowDocument::from_path(file.path()).expect("workflow should load from path");

		assert_eq!(document.frontmatter().tracker().project_slug(), "pubfi");
	}

	#[test]
	fn rejects_missing_frontmatter() {
		let result = WorkflowDocument::parse_markdown("Read `AGENTS.md` first.");

		assert!(result.is_err());
	}
}
