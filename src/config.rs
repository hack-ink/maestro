//! Service configuration for Maestro.

use std::{
	env, fs,
	net::ToSocketAddrs,
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
	github: ProjectGitHubConfig,
	#[serde(default)]
	agent: ProjectAgentConfig,
	operator_http: Option<ProjectOperatorHttpConfig>,
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

	/// GitHub configuration for this project.
	pub fn github(&self) -> &ProjectGitHubConfig {
		&self.github
	}

	/// Agent defaults scoped to this project.
	pub fn agent(&self) -> &ProjectAgentConfig {
		&self.agent
	}

	/// Optional operator HTTP status endpoint configuration.
	pub fn operator_http(&self) -> Option<&ProjectOperatorHttpConfig> {
		self.operator_http.as_ref()
	}

	fn validate(&self) -> Result<()> {
		if self.id.trim().is_empty() {
			eyre::bail!("Project id must not be empty.");
		}

		self.tracker.validate()?;

		if let Some(operator_http) = self.operator_http() {
			operator_http.validate()?;
		}

		self.github.validate()
	}
}

/// Tracker-specific settings for a target project.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectTrackerConfig {
	project_slug: String,
	api_key_env_var: String,
}
impl ProjectTrackerConfig {
	/// Stable Linear project slug.
	pub fn project_slug(&self) -> &str {
		&self.project_slug
	}

	/// Name of the environment variable that stores the tracker API key.
	pub fn api_key_env_var(&self) -> &str {
		&self.api_key_env_var
	}

	/// Resolve the configured tracker API key env-var name into a concrete token string.
	pub fn resolve_api_key(&self) -> Result<String> {
		resolve_secret_env_var("tracker.api_key_env_var", self.api_key_env_var())
	}

	fn validate(&self) -> Result<()> {
		if self.project_slug.trim().is_empty() {
			eyre::bail!("`tracker.project_slug` must not be empty.");
		}

		validate_env_var_name("tracker.api_key_env_var", self.api_key_env_var())?;

		Ok(())
	}
}

/// Optional GitHub settings for a target project.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectGitHubConfig {
	token_env_var: Option<String>,
}
impl ProjectGitHubConfig {
	/// Name of the environment variable that stores the GitHub token.
	pub fn token_env_var(&self) -> Option<&str> {
		self.token_env_var.as_deref()
	}

	fn validate(&self) -> Result<()> {
		if let Some(token_env_var) = self.token_env_var() {
			validate_env_var_name("github.token_env_var", token_env_var)?;
		}

		Ok(())
	}
}

/// Project-level agent defaults from service configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectAgentConfig {
	transport: Option<String>,
}
impl ProjectAgentConfig {
	/// Optional app-server transport override for this project.
	pub fn transport(&self) -> Option<&str> {
		self.transport.as_deref()
	}
}

/// Optional operator HTTP status endpoint settings.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOperatorHttpConfig {
	listen_address: String,
}
impl ProjectOperatorHttpConfig {
	/// Socket address where the read-only operator state endpoint listens.
	pub fn listen_address(&self) -> &str {
		&self.listen_address
	}

	fn validate(&self) -> Result<()> {
		let trimmed = self.listen_address.trim();

		if trimmed.is_empty() {
			eyre::bail!("`operator_http.listen_address` must not be empty.");
		}
		if trimmed != self.listen_address {
			eyre::bail!("`operator_http.listen_address` must not include surrounding whitespace.");
		}
		if trimmed.to_socket_addrs().is_err() {
			eyre::bail!("`operator_http.listen_address` must resolve to a valid socket address.");
		}

		Ok(())
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

fn validate_env_var_name(field_name: &str, value: &str) -> Result<()> {
	let trimmed = value.trim();

	if trimmed.is_empty() {
		eyre::bail!("`{field_name}` must not be empty.");
	}
	if trimmed != value {
		eyre::bail!("`{field_name}` must not include surrounding whitespace.");
	}
	if trimmed.starts_with('$') {
		eyre::bail!(
			"`{field_name}` must name the environment variable directly, without a `$` prefix."
		);
	}

	let mut chars = trimmed.chars();
	let Some(first) = chars.next() else {
		eyre::bail!("`{field_name}` must not be empty.");
	};

	if !(first == '_' || first.is_ascii_alphabetic()) {
		eyre::bail!(
			"`{field_name}` must start with an ASCII letter or underscore and contain only ASCII letters, digits, or underscores."
		);
	}
	if chars.any(|character| !(character == '_' || character.is_ascii_alphanumeric())) {
		eyre::bail!("`{field_name}` must contain only ASCII letters, digits, or underscores.");
	}

	Ok(())
}

fn resolve_secret_env_var(field_name: &str, env_var: &str) -> Result<String> {
	validate_env_var_name(field_name, env_var)?;

	env::var(env_var).map_err(|error| {
		eyre::eyre!(
			"Failed to read environment variable `{env_var}` referenced by `{field_name}`: {error}"
		)
	})
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
				api_key_env_var = "HOME"

				[agent]
				transport = "stdio://"
			"#,
		)
		.expect("service config should parse");

		assert_eq!(config.id(), "pubfi");
		assert_eq!(config.workflow_path(), Path::new("WORKFLOW.md"));
		assert_eq!(config.tracker().project_slug(), "pubfi");
		assert!(!config.tracker().resolve_api_key().expect("HOME should resolve").is_empty());
		assert_eq!(config.agent().transport(), Some("stdio://"));
		assert!(config.operator_http().is_none());
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
				api_key_env_var = "HOME"
			"#,
		)
		.expect("temp config should be written");

		let config =
			ServiceConfig::from_path(file.path()).expect("service config should load from disk");

		assert_eq!(config.workspace_root(), Path::new("/tmp/workspaces"));
		assert!(!config.tracker().resolve_api_key().expect("HOME should resolve").is_empty());
	}

	#[test]
	fn parses_github_token_env_var_name() {
		let config = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key_env_var = "HOME"

				[github]
				token_env_var = "HOME"
			"#,
		)
		.expect("service config should parse");

		assert_eq!(config.github().token_env_var(), Some("HOME"));
	}

	#[test]
	fn parses_optional_operator_http_listener() {
		let config = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key_env_var = "HOME"

				[operator_http]
				listen_address = "127.0.0.1:8900"
			"#,
		)
		.expect("service config should parse");

		assert_eq!(
			config.operator_http().map(|operator_http| operator_http.listen_address()),
			Some("127.0.0.1:8900")
		);
	}

	#[test]
	fn rejects_blank_operator_http_listener_address() {
		let error = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key_env_var = "HOME"

				[operator_http]
				listen_address = ""
			"#,
		)
		.expect_err("blank operator http listener should be rejected");

		assert!(error.to_string().contains("operator_http.listen_address"));
	}

	#[test]
	fn rejects_empty_github_token_env_var_when_present() {
		let result = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key_env_var = "HOME"

				[github]
				token_env_var = ""
			"#,
		);
		let error = result.expect_err("empty github token env-var should be rejected");

		assert!(error.to_string().contains("github.token_env_var"));
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
				api_key_env_var = "HOME"
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
				api_key_env_var = "HOME"
			"#,
		);
		let error =
			result.expect_err("legacy `project` key should be rejected even with `project_slug`");

		assert!(error.to_string().contains("unknown field `project`"));
	}

	#[test]
	fn rejects_legacy_tracker_api_key_field() {
		let result = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/pubfi/.workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key = "$HOME"
			"#,
		);
		let error = result.expect_err("legacy `api_key` key should be rejected");

		assert!(error.to_string().contains("unknown field `api_key`"));
	}

	#[test]
	fn rejects_dollar_prefixed_tracker_api_key_env_var() {
		let result = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/pubfi/.workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key_env_var = "$HOME"
			"#,
		);
		let error = result.expect_err("dollar-prefixed env var name should be rejected");

		assert!(error.to_string().contains("without a `$` prefix"));
	}

	#[test]
	fn rejects_legacy_github_token_field() {
		let result = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/pubfi/.workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key_env_var = "HOME"

				[github]
				token = "$HOME"
			"#,
		);
		let error = result.expect_err("legacy `token` key should be rejected");

		assert!(error.to_string().contains("unknown field `token`"));
	}

	#[test]
	fn missing_github_token_env_var_remains_unconfigured() {
		let config = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/pubfi/.workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key_env_var = "HOME"
			"#,
		)
		.expect("service config should parse");

		assert_eq!(config.github().token_env_var(), None);
	}

	#[test]
	fn rejects_legacy_agent_model_field() {
		let result = ServiceConfig::parse_toml(
			r#"
				id = "pubfi"
				repo_root = "/tmp/pubfi"
				workspace_root = "/tmp/pubfi/.workspaces"

				[tracker]
				project_slug = "pubfi"
				api_key_env_var = "HOME"

				[agent]
				transport = "stdio://"
				model = "gpt-5.4"
			"#,
		);
		let error = result.expect_err("legacy `agent.model` key should be rejected");

		assert!(error.to_string().contains("unknown field `model`"));
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
				api_key_env_var = "HOME"
			"#,
		);

		assert!(result.is_err());
	}
}
