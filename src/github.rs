use std::process::Command;

use crate::prelude::{Result, eyre};

#[derive(Debug)]
pub(crate) struct PullRequestLocator {
	pub(crate) owner: String,
	pub(crate) repo: String,
	pub(crate) number: u64,
}

pub(crate) fn configure_gh_command(command: &mut Command, github_token: Option<&str>) {
	if let Some(token) = github_token {
		command.env("GH_TOKEN", token);
	}

	command.env("GH_PROMPT_DISABLED", "1");
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

#[cfg(test)]
mod tests {
	use std::ffi::OsStr;

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
	fn configure_gh_command_sets_explicit_token_when_present() {
		let mut command = std::process::Command::new("gh");

		super::configure_gh_command(&mut command, Some("ghp_example"));

		let gh_token = command
			.get_envs()
			.find_map(|(key, value)| (key == OsStr::new("GH_TOKEN")).then_some(value))
			.flatten()
			.expect("GH_TOKEN should be injected");

		assert_eq!(gh_token, OsStr::new("ghp_example"));
	}

	#[test]
	fn configure_gh_command_preserves_standard_auth_fallback_when_token_is_missing() {
		let mut command = std::process::Command::new("gh");

		super::configure_gh_command(&mut command, None);

		assert!(
			command
				.get_envs()
				.find_map(|(key, value)| (key == OsStr::new("GH_TOKEN")).then_some(value))
				.flatten()
				.is_none(),
			"configure_gh_command should not force GH_TOKEN when no explicit token is configured"
		);
	}
}
