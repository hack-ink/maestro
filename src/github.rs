use std::{env, path::Path, process::Command};

use crate::prelude::{Result, eyre};

#[derive(Debug)]
pub(crate) struct PullRequestLocator {
	pub(crate) owner: String,
	pub(crate) repo: String,
	pub(crate) number: u64,
}

pub(crate) fn configure_gh_command(command: &mut Command, cwd: &Path) -> Result<()> {
	let identity = read_local_git_config(cwd, "codex.github-identity")?;
	let token_env = match identity.as_str() {
		"x" => "GITHUB_PAT_X",
		"y" => "GITHUB_PAT_Y",
		other => eyre::bail!(
			"Unsupported `codex.github-identity` value `{other}` in `{}`.",
			cwd.display()
		),
	};
	let token = env::var(token_env).map_err(|error| {
		eyre::eyre!(
			"Failed to read `{token_env}` for GitHub access in `{}`: {error}",
			cwd.display()
		)
	})?;

	command.env("GH_TOKEN", token);
	command.env("GH_PROMPT_DISABLED", "1");

	Ok(())
}

pub(crate) fn parse_pull_request_url(pr_url: &str) -> Result<PullRequestLocator> {
	let normalized = pr_url.trim().trim_end_matches('/');
	let suffix = normalized.strip_prefix("https://github.com/").ok_or_else(|| {
		eyre::eyre!("Pull request URL `{pr_url}` must start with `https://github.com/`.")
	})?;
	let mut segments = suffix.split('/');
	let owner = segments
		.next()
		.filter(|value| !value.is_empty())
		.ok_or_else(|| eyre::eyre!("Pull request URL `{pr_url}` is missing the owner."))?;
	let repo = segments
		.next()
		.filter(|value| !value.is_empty())
		.ok_or_else(|| eyre::eyre!("Pull request URL `{pr_url}` is missing the repository."))?;
	let pull_segment = segments
		.next()
		.ok_or_else(|| eyre::eyre!("Pull request URL `{pr_url}` is missing the `pull` segment."))?;

	if pull_segment != "pull" {
		eyre::bail!(
			"Pull request URL `{pr_url}` must use `/pull/<number>`, not `/{pull_segment}`."
		);
	}

	let number = segments
		.next()
		.ok_or_else(|| {
			eyre::eyre!("Pull request URL `{pr_url}` is missing the pull request number.")
		})?
		.parse::<u64>()
		.map_err(|error| {
			eyre::eyre!("Pull request URL `{pr_url}` has an invalid number: {error}")
		})?;

	Ok(PullRequestLocator { owner: owner.to_owned(), repo: repo.to_owned(), number })
}

fn read_local_git_config(cwd: &Path, key: &str) -> Result<String> {
	let output = Command::new("git")
		.arg("-C")
		.arg(cwd)
		.args(["config", "--local", "--get", key])
		.output()?;

	if !output.status.success() {
		let stderr = String::from_utf8_lossy(&output.stderr);

		eyre::bail!("Failed to read git config `{key}` in `{}`: {}", cwd.display(), stderr.trim());
	}

	let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();

	if value.is_empty() {
		eyre::bail!("Git config `{key}` in `{}` must not be empty.", cwd.display());
	}

	Ok(value)
}

#[cfg(test)]
mod tests {
	#[test]
	fn parses_pull_request_url() {
		let locator = super::parse_pull_request_url("https://github.com/hack-ink/maestro/pull/20")
			.expect("pull request URL should parse");

		assert_eq!(locator.owner, "hack-ink");
		assert_eq!(locator.repo, "maestro");
		assert_eq!(locator.number, 20);
	}

	#[test]
	fn rejects_non_pull_github_url() {
		let error = super::parse_pull_request_url("https://github.com/hack-ink/maestro/issues/20")
			.expect_err("issue URL should be rejected");

		assert!(error.to_string().contains("/pull/<number>"));
	}

	#[test]
	fn rejects_missing_number() {
		let error = super::parse_pull_request_url("https://github.com/hack-ink/maestro/pull/")
			.expect_err("missing pull number should be rejected");

		assert!(error.to_string().contains("missing the pull request number"));
	}

	#[test]
	fn configure_gh_command_requires_repo_local_identity_when_missing() {
		let temp_dir = tempfile::tempdir().expect("temp dir should exist");

		std::process::Command::new("git")
			.arg("-C")
			.arg(temp_dir.path())
			.args(["init"])
			.output()
			.expect("git init should run");

		let mut command = std::process::Command::new("gh");
		let error = super::configure_gh_command(&mut command, temp_dir.path())
			.expect_err("missing codex.github-identity should be rejected");

		assert!(error.to_string().contains("codex.github-identity"));
	}
}
