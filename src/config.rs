//! Service configuration for Maestro.

use std::{
	collections::BTreeSet,
	fs,
	path::{Path, PathBuf},
};

use directories::ProjectDirs;
use serde::Deserialize;

use crate::prelude::{Result, eyre};

/// Top-level service configuration for one or more target repositories.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct ServiceConfig {
	projects: Vec<ProjectConfig>,
}
impl ServiceConfig {
	/// Parse service configuration from TOML text.
	pub fn parse_toml(input: &str) -> Result<Self> {
		let config = toml::from_str::<Self>(input)?;

		config.validate()?;

		Ok(config)
	}

	/// Load service configuration from a TOML file on disk.
	pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
		let input = fs::read_to_string(path)?;

		Self::parse_toml(&input)
	}

	/// Configured target repositories.
	pub fn projects(&self) -> &[ProjectConfig] {
		&self.projects
	}

	fn validate(&self) -> Result<()> {
		if self.projects.is_empty() {
			eyre::bail!("Service configuration must include at least one project.");
		}

		let mut seen_ids = BTreeSet::new();

		for project in &self.projects {
			project.validate()?;

			if !seen_ids.insert(project.id.as_str()) {
				eyre::bail!("Duplicate project id detected: {}", project.id);
			}
		}

		Ok(())
	}
}

/// Per-repository service configuration.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct ProjectConfig {
	id: String,
	repo_root: PathBuf,
	workspace_root: PathBuf,
	#[serde(default = "default_workflow_path")]
	workflow_path: PathBuf,
	tracker: ProjectTrackerConfig,
	#[serde(default)]
	agent: ProjectAgentConfig,
}
impl ProjectConfig {
	/// Stable identifier for this project entry.
	pub fn id(&self) -> &str {
		&self.id
	}

	/// Absolute or service-relative repository root used for the target checkout.
	pub fn repo_root(&self) -> &Path {
		&self.repo_root
	}

	/// Workspace root where `maestro` creates issue lanes.
	pub fn workspace_root(&self) -> &Path {
		&self.workspace_root
	}

	/// Repository-relative path to the downstream `WORKFLOW.md`.
	pub fn workflow_path(&self) -> &Path {
		&self.workflow_path
	}

	/// Tracker configuration for this project.
	pub fn tracker(&self) -> &ProjectTrackerConfig {
		&self.tracker
	}

	/// Agent defaults scoped to this project.
	pub fn agent(&self) -> &ProjectAgentConfig {
		&self.agent
	}

	fn validate(&self) -> Result<()> {
		if self.id.trim().is_empty() {
			eyre::bail!("Project id must not be empty.");
		}

		self.tracker.validate()
	}
}

/// Tracker-specific settings for a target project.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct ProjectTrackerConfig {
	#[serde(alias = "project")]
	project_slug: String,
	api_token_env: String,
}
impl ProjectTrackerConfig {
	/// Stable Linear project slug.
	pub fn project_slug(&self) -> &str {
		&self.project_slug
	}

	/// Environment variable name containing the tracker API token.
	pub fn api_token_env(&self) -> &str {
		&self.api_token_env
	}

	fn validate(&self) -> Result<()> {
		if self.project_slug.trim().is_empty() {
			eyre::bail!("`projects.tracker.project_slug` must not be empty.");
		}
		if self.api_token_env.trim().is_empty() {
			eyre::bail!("`projects.tracker.api_token_env` must not be empty.");
		}

		Ok(())
	}
}

/// Project-level agent defaults from service configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
pub struct ProjectAgentConfig {
	transport: Option<String>,
	model: Option<String>,
}
impl ProjectAgentConfig {
	/// Optional app-server transport override for this project.
	pub fn transport(&self) -> Option<&str> {
		self.transport.as_deref()
	}

	/// Optional default model override for this project.
	pub fn model(&self) -> Option<&str> {
		self.model.as_deref()
	}
}

/// Default service configuration path under the platform config directory.
pub fn default_config_path() -> Result<PathBuf> {
	let project_dirs = ProjectDirs::from("", "helixbox", env!("CARGO_PKG_NAME"))
		.ok_or_else(|| eyre::eyre!("Failed to resolve project directories."))?;

	Ok(project_dirs.config_dir().join("maestro.toml"))
}

fn default_workflow_path() -> PathBuf {
	PathBuf::from("WORKFLOW.md")
}

#[cfg(test)]
mod tests {
	use std::{fs, path::Path};

	use tempfile::NamedTempFile;

	use crate::config::ServiceConfig;

	#[test]
	fn parses_service_config_from_str() {
		let config = ServiceConfig::parse_toml(
			r#"
				[[projects]]
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/maestro-workspaces/pubfi"

				[projects.tracker]
				project_slug = "pubfi"
				api_token_env = "LINEAR_API_KEY"

				[projects.agent]
				transport = "stdio://"
				model = "gpt-5-codex"
			"#,
		)
		.expect("service config should parse");

		assert_eq!(config.projects().len(), 1);
		assert_eq!(config.projects()[0].id(), "pubfi");
		assert_eq!(config.projects()[0].workflow_path(), Path::new("WORKFLOW.md"));
		assert_eq!(config.projects()[0].tracker().project_slug(), "pubfi");
		assert_eq!(config.projects()[0].agent().model(), Some("gpt-5-codex"));
	}

	#[test]
	fn loads_service_config_from_path() {
		let file = NamedTempFile::new().expect("temp file should exist");

		fs::write(
			file.path(),
			r#"
				[[projects]]
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/workspaces"

				[projects.tracker]
				project_slug = "pubfi"
				api_token_env = "LINEAR_API_KEY"
			"#,
		)
		.expect("temp config should be written");

		let config =
			ServiceConfig::from_path(file.path()).expect("service config should load from disk");

		assert_eq!(config.projects()[0].workspace_root(), Path::new("/tmp/workspaces"));
	}

	#[test]
	fn rejects_duplicate_project_ids() {
		let result = ServiceConfig::parse_toml(
			r#"
				[[projects]]
				id = "dup"
				repo_root = "/tmp/one"
				workspace_root = "/tmp/workspaces/one"

				[projects.tracker]
				project_slug = "one"
				api_token_env = "LINEAR_API_KEY"

				[[projects]]
				id = "dup"
				repo_root = "/tmp/two"
				workspace_root = "/tmp/workspaces/two"

				[projects.tracker]
				project_slug = "two"
				api_token_env = "LINEAR_API_KEY"
			"#,
		);

		assert!(result.is_err());
	}
}
