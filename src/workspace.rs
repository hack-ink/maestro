use std::{
	fs,
	path::{Path, PathBuf},
	process::Command,
};

use crate::prelude::{Result, eyre};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceSpec {
	pub(crate) branch_name: String,
	pub(crate) issue_identifier: String,
	pub(crate) path: PathBuf,
	pub(crate) reused_existing: bool,
}

pub(crate) struct WorkspaceManager {
	repo_root: PathBuf,
	workspace_root: PathBuf,
	project_id: String,
}
impl WorkspaceManager {
	pub(crate) fn new(
		project_id: impl Into<String>,
		repo_root: impl Into<PathBuf>,
		workspace_root: impl Into<PathBuf>,
	) -> Self {
		Self {
			repo_root: repo_root.into(),
			workspace_root: workspace_root.into(),
			project_id: project_id.into(),
		}
	}

	pub(crate) fn plan_for_issue(&self, issue_identifier: &str) -> WorkspaceSpec {
		let branch_suffix = sanitize_branch_component(issue_identifier);
		let branch_name =
			format!("x/{}-{}", sanitize_branch_component(&self.project_id), branch_suffix);
		let path = self.workspace_root.join(issue_identifier);
		let reused_existing = path.join(".git").exists();

		WorkspaceSpec {
			branch_name,
			issue_identifier: issue_identifier.to_owned(),
			path,
			reused_existing,
		}
	}

	pub(crate) fn ensure_workspace(
		&self,
		issue_identifier: &str,
		dry_run: bool,
	) -> Result<WorkspaceSpec> {
		let spec = self.plan_for_issue(issue_identifier);

		if dry_run || spec.reused_existing {
			return Ok(spec);
		}

		fs::create_dir_all(&self.workspace_root)?;

		let mut command = Command::new("git");

		command.arg("-C").arg(&self.repo_root).arg("worktree").arg("add");

		if branch_exists(&self.repo_root, &spec.branch_name)? {
			command.arg(&spec.path).arg(&spec.branch_name);
		} else {
			command.arg("-b").arg(&spec.branch_name).arg(&spec.path).arg("HEAD");
		}

		let output = command.output()?;

		if !output.status.success() {
			let stderr = String::from_utf8_lossy(&output.stderr);

			eyre::bail!("Failed to create worktree `{}`: {}", spec.path.display(), stderr.trim());
		}

		Ok(spec)
	}

	pub(crate) fn remove_workspace_path(&self, path: &Path) -> Result<bool> {
		if !path.exists() {
			return Ok(false);
		}

		let output = Command::new("git")
			.arg("-C")
			.arg(&self.repo_root)
			.arg("worktree")
			.arg("remove")
			.arg("--force")
			.arg(path)
			.output()?;

		if !output.status.success() {
			let stderr = String::from_utf8_lossy(&output.stderr);

			eyre::bail!("Failed to remove worktree `{}`: {}", path.display(), stderr.trim());
		}

		Ok(true)
	}
}

fn branch_exists(repo_root: &Path, branch_name: &str) -> Result<bool> {
	let output = Command::new("git")
		.arg("-C")
		.arg(repo_root)
		.args(["rev-parse", "--verify", "--quiet"])
		.arg(format!("refs/heads/{branch_name}"))
		.output()?;

	Ok(output.status.success())
}

fn sanitize_branch_component(value: &str) -> String {
	value
		.chars()
		.map(|ch| match ch {
			'A'..='Z' => ch.to_ascii_lowercase(),
			'a'..='z' | '0'..='9' => ch,
			'-' | '_' => '-',
			_ => '-',
		})
		.collect::<String>()
		.trim_matches('-')
		.to_owned()
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use crate::workspace::WorkspaceManager;

	#[test]
	fn plans_workspace_paths_and_branch_names() {
		let manager = WorkspaceManager::new("pubfi", "/tmp/pubfi", "/tmp/maestro-workspaces/pubfi");
		let spec = manager.plan_for_issue("PUB-101");

		assert_eq!(spec.branch_name, "x/pubfi-pub-101");
		assert_eq!(spec.path, Path::new("/tmp/maestro-workspaces/pubfi/PUB-101"));
		assert!(!spec.reused_existing);
	}
}
