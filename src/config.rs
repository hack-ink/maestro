//! Service configuration for Maestro.

use std::{
	env, fs,
	path::{Path, PathBuf},
};

use directories::ProjectDirs;
use serde::Deserialize;

use crate::prelude::{Result, eyre};

/// Top-level service configuration for one target repository and tracker scope.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct ServiceConfig {
	id: String,
	repo_root: PathBuf,
	workspace_root: PathBuf,
	#[serde(default = "default_workflow_path")]
	workflow_path: PathBuf,
	tracker: ProjectTrackerConfig,
	#[serde(default)]
	agent: ProjectAgentConfig,
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

	/// Stable identifier for this target service config.
	pub fn id(&self) -> &str {
		&self.id
	}

	/// Absolute repository root used for the target checkout.
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
#[serde(deny_unknown_fields)]
pub struct ProjectTrackerConfig {
	project_slug: String,
	api_key: String,
}
impl ProjectTrackerConfig {
	/// Stable Linear project slug.
	pub fn project_slug(&self) -> &str {
		&self.project_slug
	}

	/// Tracker API key value or environment-variable reference like `$LINEAR_API_KEY`.
	pub fn api_key(&self) -> &str {
		&self.api_key
	}

	/// Resolve the configured tracker API key into a concrete token string.
	pub fn resolve_api_key(&self) -> Result<String> {
		let api_key = self.api_key();

		if let Some(env_var) = api_key.strip_prefix('$') {
			if env_var.trim().is_empty() {
				eyre::bail!("`tracker.api_key` env reference must include a variable name.");
			}

			return env::var(env_var).map_err(|error| {
				eyre::eyre!(
					"Failed to read environment variable `{env_var}` referenced by `tracker.api_key`: {error}"
				)
			});
		}

		Ok(api_key.to_owned())
	}

	fn validate(&self) -> Result<()> {
		if self.project_slug.trim().is_empty() {
			eyre::bail!("`tracker.project_slug` must not be empty.");
		}
		if self.api_key.trim().is_empty() {
			eyre::bail!("`tracker.api_key` must not be empty.");
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
				id = "pubfi"
				repo_root = "/tmp/pubfi"
					workspace_root = "/tmp/pubfi/.workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key = "lin_api_test"

				[agent]
				transport = "stdio://"
				model = "gpt-5-codex"
			"#,
		)
		.expect("service config should parse");

		assert_eq!(config.id(), "pubfi");
		assert_eq!(config.workflow_path(), Path::new("WORKFLOW.md"));
		assert_eq!(config.tracker().project_slug(), "pubfi");
		assert_eq!(
			config.tracker().resolve_api_key().expect("literal key should resolve"),
			"lin_api_test"
		);
		assert_eq!(config.agent().model(), Some("gpt-5-codex"));
	}

	#[test]
	fn loads_service_config_from_path() {
		let file = NamedTempFile::new().expect("temp file should exist");

		fs::write(
			file.path(),
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key = "$HOME"
			"#,
		)
		.expect("temp config should be written");

		let config =
			ServiceConfig::from_path(file.path()).expect("service config should load from disk");

		assert_eq!(config.workspace_root(), Path::new("/tmp/workspaces"));
		assert!(!config.tracker().resolve_api_key().expect("HOME should resolve").is_empty());
	}

	#[test]
	fn rejects_legacy_project_alias_in_service_config() {
		let result = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/pubfi/.workspaces"

				[tracker]
				project = "pubfi"
				api_key = "lin_api_test"
			"#,
		);
		let error = result.expect_err("legacy `project` key should be rejected");

		assert!(error.to_string().contains("unknown field `project`"));
	}

	#[test]
	fn rejects_legacy_project_key_when_project_slug_is_present() {
		let result = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/pubfi/.workspaces"

				[tracker]
				project_slug = "pubfi"
				project = "legacy-pubfi"
				api_key = "lin_api_test"
			"#,
		);
		let error =
			result.expect_err("legacy `project` key should be rejected even with `project_slug`");

		assert!(error.to_string().contains("unknown field `project`"));
	}

	#[test]
	fn rejects_empty_project_id() {
		let result = ServiceConfig::parse_toml(
			r#"
				id = ""
				repo_root = "/tmp/one"
				workspace_root = "/tmp/workspaces/one"

				[tracker]
				project_slug = "one"
				api_key = "lin_api_test"
			"#,
		);

		assert!(result.is_err());
	}
}
