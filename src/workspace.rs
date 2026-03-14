use std::{
	ffi::OsStr,
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

		if dry_run {
			return Ok(spec);
		}
		if spec.reused_existing {
			self.validate_workspace_boundary(&spec.path)?;
			self.refresh_workspace_git_metadata(&spec.path)?;

			return Ok(spec);
		}

		fs::create_dir_all(&self.workspace_root)?;

		self.create_clone_backed_workspace(&spec)?;
		self.validate_workspace_boundary(&spec.path)?;

		Ok(spec)
	}

	pub(crate) fn remove_workspace_path(&self, path: &Path) -> Result<bool> {
		if !path.exists() {
			return Ok(false);
		}

		let workspace_root = fs::canonicalize(&self.workspace_root)?;
		let canonical_path = fs::canonicalize(path)?;

		if !canonical_path.starts_with(&workspace_root) || canonical_path == workspace_root {
			eyre::bail!(
				"Refusing to remove workspace `{}` outside workspace_root `{}`.",
				path.display(),
				self.workspace_root.display()
			);
		}

		fs::remove_dir_all(&canonical_path)?;

		Ok(true)
	}

	fn create_clone_backed_workspace(&self, spec: &WorkspaceSpec) -> Result<()> {
		let source_head =
			git_stdout(&self.repo_root, ["rev-parse", "HEAD"], "read the source repository HEAD")?;

		if spec.path.exists() {
			eyre::bail!(
				"Workspace path `{}` already exists but does not look reusable.",
				spec.path.display()
			);
		}

		let clone_output = Command::new("git")
			.arg("-C")
			.arg(&self.repo_root)
			.args(["clone", "--quiet", "--no-checkout", "."])
			.arg(&spec.path)
			.output()?;

		if !clone_output.status.success() {
			let _ = fs::remove_dir_all(&spec.path);
			let stderr = String::from_utf8_lossy(&clone_output.stderr);

			eyre::bail!(
				"Failed to clone workspace `{}` from `{}`: {}",
				spec.path.display(),
				self.repo_root.display(),
				stderr.trim()
			);
		}

		let setup_result = (|| -> Result<()> {
			self.refresh_workspace_git_metadata(&spec.path)?;
			self.checkout_workspace_branch(&spec.path, spec.branch_name.as_str(), &source_head)?;

			Ok(())
		})();

		if let Err(error) = setup_result {
			let _ = fs::remove_dir_all(&spec.path);

			return Err(error);
		}

		Ok(())
	}

	fn refresh_workspace_git_metadata(&self, workspace_path: &Path) -> Result<()> {
		copy_repo_local_git_config(&self.repo_root, workspace_path)?;

		if let Some(source_origin_url) = try_git_stdout(
			&self.repo_root,
			["remote", "get-url", "origin"],
			"read the source repository origin URL",
		)?
		.map(|url| normalize_remote_url(&self.repo_root, &url))
		{
			run_git(
				workspace_path,
				["remote", "set-url", "origin", source_origin_url.as_str()],
				"rewrite the workspace origin remote",
			)?;
		} else {
			remove_git_remote_if_present(workspace_path, "origin")?;
		}

		Ok(())
	}

	fn checkout_workspace_branch(
		&self,
		workspace_path: &Path,
		branch_name: &str,
		source_head: &str,
	) -> Result<()> {
		if fetch_remote_branch_if_present(workspace_path, branch_name)? {
			let remote_tracking_ref = format!("refs/remotes/origin/{branch_name}");

			run_git(
				workspace_path,
				["checkout", "--quiet", "-B", branch_name, remote_tracking_ref.as_str()],
				"checkout the workspace branch from the remote lane head",
			)?;
		} else {
			run_git(
				workspace_path,
				["checkout", "--quiet", "-B", branch_name, source_head],
				"checkout the workspace branch",
			)?;
		}

		Ok(())
	}

	fn validate_workspace_boundary(&self, workspace_path: &Path) -> Result<()> {
		let workspace_root = fs::canonicalize(workspace_path)?;
		let git_dir = git_stdout(
			workspace_path,
			["rev-parse", "--path-format=absolute", "--git-dir"],
			"resolve workspace git dir",
		)?;
		let git_common_dir = git_stdout(
			workspace_path,
			["rev-parse", "--path-format=absolute", "--git-common-dir"],
			"resolve workspace git common dir",
		)?;
		let git_dir = fs::canonicalize(PathBuf::from(git_dir))?;
		let git_common_dir = fs::canonicalize(PathBuf::from(git_common_dir))?;

		if !git_dir.starts_with(&workspace_root) {
			eyre::bail!(
				"Workspace `{}` is not self-contained: git dir `{}` escapes the workspace root.",
				workspace_path.display(),
				git_dir.display()
			);
		}
		if !git_common_dir.starts_with(&workspace_root) {
			eyre::bail!(
				"Workspace `{}` is not self-contained: git common dir `{}` escapes the workspace root.",
				workspace_path.display(),
				git_common_dir.display()
			);
		}

		Ok(())
	}
}

fn git_stdout<I, S>(repo_root: &Path, args: I, action: &str) -> Result<String>
where
	I: IntoIterator<Item = S>,
	S: AsRef<OsStr>,
{
	let output = Command::new("git").arg("-C").arg(repo_root).args(args).output()?;

	if !output.status.success() {
		let stderr = String::from_utf8_lossy(&output.stderr);

		eyre::bail!("Failed to {action} in `{}`: {}", repo_root.display(), stderr.trim());
	}

	Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn try_git_stdout<I, S>(repo_root: &Path, args: I, action: &str) -> Result<Option<String>>
where
	I: IntoIterator<Item = S>,
	S: AsRef<OsStr>,
{
	let output = Command::new("git").arg("-C").arg(repo_root).args(args).output()?;

	if output.status.success() {
		return Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()));
	}

	let stderr = String::from_utf8_lossy(&output.stderr);

	if stderr.contains("No such remote") {
		return Ok(None);
	}

	eyre::bail!("Failed to {action} in `{}`: {}", repo_root.display(), stderr.trim());
}

fn run_git<I, S>(repo_root: &Path, args: I, action: &str) -> Result<()>
where
	I: IntoIterator<Item = S>,
	S: AsRef<OsStr>,
{
	let output = Command::new("git").arg("-C").arg(repo_root).args(args).output()?;

	if !output.status.success() {
		let stderr = String::from_utf8_lossy(&output.stderr);

		eyre::bail!("Failed to {action} in `{}`: {}", repo_root.display(), stderr.trim());
	}

	Ok(())
}

fn fetch_remote_branch_if_present(workspace_path: &Path, branch_name: &str) -> Result<bool> {
	if try_git_stdout(
		workspace_path,
		["remote", "get-url", "origin"],
		"read workspace origin remote",
	)?
	.is_none()
	{
		return Ok(false);
	}

	let remote_ref = format!("refs/heads/{branch_name}");
	let branch_check = Command::new("git")
		.arg("-C")
		.arg(workspace_path)
		.args(["ls-remote", "--exit-code", "--heads", "origin", remote_ref.as_str()])
		.output()?;

	if !branch_check.status.success() {
		if branch_check.status.code() == Some(2) {
			return Ok(false);
		}

		let stderr = String::from_utf8_lossy(&branch_check.stderr);

		eyre::bail!(
			"Failed to inspect remote workspace branch `{branch_name}` in `{}`: {}",
			workspace_path.display(),
			stderr.trim()
		);
	}

	let remote_tracking_ref = format!("refs/remotes/origin/{branch_name}");
	let output = Command::new("git")
		.arg("-C")
		.arg(workspace_path)
		.args([
			"fetch",
			"--quiet",
			"--no-tags",
			"origin",
			&format!("refs/heads/{branch_name}:{remote_tracking_ref}"),
		])
		.output()?;

	if output.status.success() {
		return Ok(true);
	}

	let stderr = String::from_utf8_lossy(&output.stderr);

	eyre::bail!(
		"Failed to fetch remote workspace branch `{branch_name}` in `{}`: {}",
		workspace_path.display(),
		stderr.trim()
	);
}

fn remove_git_remote_if_present(repo_root: &Path, remote_name: &str) -> Result<()> {
	let output = Command::new("git")
		.arg("-C")
		.arg(repo_root)
		.args(["remote", "remove", remote_name])
		.output()?;

	if output.status.success() {
		return Ok(());
	}

	let stderr = String::from_utf8_lossy(&output.stderr);

	if stderr.contains("No such remote") {
		return Ok(());
	}

	eyre::bail!(
		"Failed to remove git remote `{remote_name}` in `{}`: {}",
		repo_root.display(),
		stderr.trim()
	);
}

fn copy_repo_local_git_config(source_repo_root: &Path, workspace_path: &Path) -> Result<()> {
	clear_managed_workspace_git_config(workspace_path)?;

	let local_entries = git_stdout(
		source_repo_root,
		["config", "--local", "--null", "--list"],
		"read source repository local git config",
	)?;

	for raw_entry in local_entries.split('\0').filter(|entry| !entry.is_empty()) {
		let Some((key, value)) = raw_entry.split_once('\n') else {
			continue;
		};

		if !should_copy_local_git_config(key) {
			continue;
		}

		run_git(
			workspace_path,
			["config", "--local", "--add", key, value],
			"copy source repository local git config into the workspace",
		)?;
	}

	Ok(())
}

fn clear_managed_workspace_git_config(workspace_path: &Path) -> Result<()> {
	let local_entries = git_stdout(
		workspace_path,
		["config", "--local", "--null", "--list"],
		"read workspace local git config",
	)?;
	let mut managed_keys = Vec::new();

	for raw_entry in local_entries.split('\0').filter(|entry| !entry.is_empty()) {
		let Some((key, _value)) = raw_entry.split_once('\n') else {
			continue;
		};

		if should_copy_local_git_config(key) && !managed_keys.iter().any(|item| item == key) {
			managed_keys.push(key.to_owned());
		}
	}
	for key in managed_keys {
		unset_all_local_config(workspace_path, &key)?;
	}

	Ok(())
}

fn unset_all_local_config(repo_root: &Path, key: &str) -> Result<()> {
	let output = Command::new("git")
		.arg("-C")
		.arg(repo_root)
		.args(["config", "--local", "--unset-all", key])
		.output()?;

	if output.status.success() {
		return Ok(());
	}

	match output.status.code() {
		Some(5) => Ok(()),
		_ => {
			let stderr = String::from_utf8_lossy(&output.stderr);

			eyre::bail!(
				"Failed to clear workspace local git config `{key}` in `{}`: {}",
				repo_root.display(),
				stderr.trim()
			);
		},
	}
}

fn normalize_remote_url(source_repo_root: &Path, remote_url: &str) -> String {
	if !is_relative_path_remote(remote_url) {
		return remote_url.to_owned();
	}

	let resolved = source_repo_root.join(remote_url);

	fs::canonicalize(&resolved).unwrap_or(resolved).display().to_string()
}

fn should_copy_local_git_config(key: &str) -> bool {
	key.starts_with("user.")
		|| key.starts_with("gpg.")
		|| matches!(key, "commit.gpgsign" | "tag.gpgsign")
}

fn is_relative_path_remote(remote_url: &str) -> bool {
	let path = Path::new(remote_url);

	!path.is_absolute()
		&& !remote_url.contains("://")
		&& !looks_like_windows_absolute_path(remote_url)
		&& !looks_like_scp_remote(remote_url)
}

fn looks_like_windows_absolute_path(remote_url: &str) -> bool {
	let bytes = remote_url.as_bytes();

	bytes.len() >= 3
		&& bytes[0].is_ascii_alphabetic()
		&& bytes[1] == b':'
		&& matches!(bytes[2], b'/' | b'\\')
}

fn looks_like_scp_remote(remote_url: &str) -> bool {
	let Some(colon_index) = remote_url.find(':') else {
		return false;
	};
	let slash_index = remote_url.find('/').unwrap_or(usize::MAX);
	let backslash_index = remote_url.find('\\').unwrap_or(usize::MAX);

	colon_index < slash_index.min(backslash_index)
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
	use std::{
		fs,
		path::{Path, PathBuf},
		process::Command,
	};

	use tempfile::TempDir;

	use crate::workspace::WorkspaceManager;

	fn run_git(repo_root: &Path, args: &[&str]) {
		let output = Command::new("git")
			.arg("-C")
			.arg(repo_root)
			.args(args)
			.output()
			.expect("git command should run");

		assert!(
			output.status.success(),
			"git {:?} failed in {}: {}",
			args,
			repo_root.display(),
			String::from_utf8_lossy(&output.stderr)
		);
	}

	fn git_stdout(repo_root: &Path, args: &[&str]) -> String {
		let output = Command::new("git")
			.arg("-C")
			.arg(repo_root)
			.args(args)
			.output()
			.expect("git command should run");

		assert!(
			output.status.success(),
			"git {:?} failed in {}: {}",
			args,
			repo_root.display(),
			String::from_utf8_lossy(&output.stderr)
		);

		String::from_utf8_lossy(&output.stdout).trim().to_owned()
	}

	fn init_repo() -> (TempDir, PathBuf) {
		let temp_dir = TempDir::new().expect("temp dir should exist");
		let repo_root = temp_dir.path().join("repo");
		let default_origin = repo_root.parent().unwrap().join("source-origin.git");

		fs::create_dir_all(&repo_root).expect("repo root should exist");

		run_git(
			default_origin.parent().unwrap(),
			&["init", "--bare", default_origin.to_str().unwrap()],
		);
		run_git(&repo_root, &["init", "--initial-branch", "main"]);
		run_git(&repo_root, &["config", "user.name", "Maestro Tests"]);
		run_git(&repo_root, &["config", "user.email", "maestro-tests@example.com"]);
		run_git(&repo_root, &["remote", "add", "origin", default_origin.to_str().unwrap()]);

		fs::write(repo_root.join("README.md"), "hello\n").expect("seed file should write");

		run_git(&repo_root, &["add", "README.md"]);
		run_git(&repo_root, &["commit", "-m", "seed"]);

		(temp_dir, repo_root)
	}

	#[test]
	fn plans_workspace_paths_and_branch_names() {
		let manager = WorkspaceManager::new("pubfi", "/tmp/pubfi", "/tmp/pubfi/.workspaces");
		let spec = manager.plan_for_issue("PUB-101");

		assert_eq!(spec.branch_name, "x/pubfi-pub-101");
		assert_eq!(spec.path, Path::new("/tmp/pubfi/.workspaces/PUB-101"));
		assert!(!spec.reused_existing);
	}

	#[test]
	fn creates_clone_backed_workspace() {
		let (_temp_dir, repo_root) = init_repo();
		let workspace_root = repo_root.join(".workspaces");
		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);
		let spec = manager.ensure_workspace("PUB-101", false).expect("workspace should be created");

		assert_eq!(spec.branch_name, "x/pubfi-pub-101");
		assert!(spec.path.join(".git").exists());
		assert_eq!(
			git_stdout(&spec.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
			"x/pubfi-pub-101"
		);
		assert_eq!(
			git_stdout(&spec.path, &["remote", "get-url", "origin"]),
			repo_root.parent().unwrap().join("source-origin.git").display().to_string()
		);

		let git_dir = PathBuf::from(git_stdout(
			&spec.path,
			&["rev-parse", "--path-format=absolute", "--git-dir"],
		));
		let git_common_dir = PathBuf::from(git_stdout(
			&spec.path,
			&["rev-parse", "--path-format=absolute", "--git-common-dir"],
		));
		let workspace_root =
			fs::canonicalize(&spec.path).expect("workspace path should canonicalize");
		let git_dir = fs::canonicalize(git_dir).expect("git dir should canonicalize");
		let git_common_dir =
			fs::canonicalize(git_common_dir).expect("git common dir should canonicalize");

		assert!(git_dir.starts_with(&workspace_root));
		assert!(git_common_dir.starts_with(&workspace_root));
	}

	#[test]
	fn clone_backed_workspace_copies_repo_local_identity_config() {
		let (_temp_dir, repo_root) = init_repo();
		let workspace_root = repo_root.join(".workspaces");
		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);

		run_git(&repo_root, &["config", "commit.gpgsign", "false"]);
		run_git(&repo_root, &["config", "user.signingkey", "workspace-tests"]);

		let spec = manager.ensure_workspace("PUB-101", false).expect("workspace should be created");

		assert_eq!(
			git_stdout(&spec.path, &["config", "--local", "--get", "user.name"]),
			"Maestro Tests"
		);
		assert_eq!(
			git_stdout(&spec.path, &["config", "--local", "--get", "user.email"]),
			"maestro-tests@example.com"
		);
		assert_eq!(
			git_stdout(&spec.path, &["config", "--local", "--get", "commit.gpgsign"]),
			"false"
		);
		assert_eq!(
			git_stdout(&spec.path, &["config", "--local", "--get", "user.signingkey"]),
			"workspace-tests"
		);
	}

	#[test]
	fn clone_backed_workspace_normalizes_relative_origin_remote() {
		let (_temp_dir, repo_root) = init_repo();
		let bare_remote = repo_root.parent().unwrap().join("relative-remote.git");
		let workspace_root = repo_root.join(".workspaces");
		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);

		run_git(bare_remote.parent().unwrap(), &["init", "--bare", bare_remote.to_str().unwrap()]);
		run_git(&repo_root, &["remote", "set-url", "origin", "../relative-remote.git"]);

		let spec = manager.ensure_workspace("PUB-101", false).expect("workspace should be created");

		assert_eq!(
			git_stdout(&spec.path, &["remote", "get-url", "origin"]),
			fs::canonicalize(&bare_remote)
				.expect("bare remote should canonicalize")
				.display()
				.to_string()
		);

		run_git(&spec.path, &["ls-remote", "origin"]);
	}

	#[test]
	fn clone_backed_workspace_removes_default_origin_when_source_has_no_origin() {
		let (_temp_dir, repo_root) = init_repo();
		let workspace_root = repo_root.join(".workspaces");
		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);

		run_git(&repo_root, &["remote", "remove", "origin"]);

		let spec = manager.ensure_workspace("PUB-101", false).expect("workspace should be created");

		assert_eq!(git_stdout(&spec.path, &["remote"]), "");
	}

	#[test]
	fn clone_backed_workspace_uses_existing_remote_lane_branch_when_present() {
		let (_temp_dir, repo_root) = init_repo();
		let bare_remote = repo_root.parent().unwrap().join("lane-remote.git");
		let workspace_root = repo_root.join(".workspaces");
		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);
		let lane_branch = "x/pubfi-pub-101";

		run_git(bare_remote.parent().unwrap(), &["init", "--bare", bare_remote.to_str().unwrap()]);
		run_git(&repo_root, &["remote", "set-url", "origin", bare_remote.to_str().unwrap()]);
		run_git(&repo_root, &["push", "-u", "origin", "main"]);
		run_git(&repo_root, &["checkout", "-b", lane_branch]);

		fs::write(repo_root.join("LANE.md"), "lane branch\n").expect("lane file should write");

		run_git(&repo_root, &["add", "LANE.md"]);
		run_git(&repo_root, &["commit", "-m", "lane branch"]);
		run_git(&repo_root, &["push", "-u", "origin", lane_branch]);
		run_git(&repo_root, &["checkout", "main"]);

		let spec = manager.ensure_workspace("PUB-101", false).expect("workspace should be created");

		assert_eq!(git_stdout(&spec.path, &["rev-parse", "--abbrev-ref", "HEAD"]), lane_branch);
		assert_eq!(
			fs::read_to_string(spec.path.join("LANE.md")).expect("lane file should exist"),
			"lane branch\n"
		);
	}

	#[test]
	fn clone_backed_workspace_fails_when_remote_branch_probe_errors() {
		let (_temp_dir, repo_root) = init_repo();
		let workspace_root = repo_root.join(".workspaces");
		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);

		run_git(
			&repo_root,
			&["remote", "set-url", "origin", "https://github.com/example/maestro.git"],
		);

		let error = manager
			.ensure_workspace("PUB-101", false)
			.expect_err("workspace create should fail when remote probe errors");

		assert!(error.to_string().contains("Failed to inspect remote workspace branch"));
	}

	#[test]
	fn reused_clone_backed_workspace_refreshes_git_metadata_from_source_repo() {
		let (_temp_dir, repo_root) = init_repo();
		let first_remote = repo_root.parent().unwrap().join("remote-one.git");
		let second_remote = repo_root.parent().unwrap().join("remote-two.git");
		let workspace_root = repo_root.join(".workspaces");
		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);

		run_git(
			first_remote.parent().unwrap(),
			&["init", "--bare", first_remote.to_str().unwrap()],
		);
		run_git(
			second_remote.parent().unwrap(),
			&["init", "--bare", second_remote.to_str().unwrap()],
		);
		run_git(&repo_root, &["remote", "set-url", "origin", "../remote-one.git"]);
		run_git(&repo_root, &["config", "user.name", "Initial Tests"]);

		let initial_spec =
			manager.ensure_workspace("PUB-101", false).expect("initial workspace should exist");

		assert_eq!(
			git_stdout(&initial_spec.path, &["config", "--local", "--get", "user.name"]),
			"Initial Tests"
		);
		assert_eq!(
			git_stdout(&initial_spec.path, &["remote", "get-url", "origin"]),
			fs::canonicalize(&first_remote)
				.expect("first remote should canonicalize")
				.display()
				.to_string()
		);

		run_git(&repo_root, &["config", "user.name", "Updated Tests"]);
		run_git(&repo_root, &["config", "user.email", "updated@example.com"]);
		run_git(&repo_root, &["remote", "set-url", "origin", "../remote-two.git"]);

		let reused_spec =
			manager.ensure_workspace("PUB-101", false).expect("reused workspace should exist");

		assert!(reused_spec.reused_existing);
		assert_eq!(
			git_stdout(&reused_spec.path, &["config", "--local", "--get", "user.name"]),
			"Updated Tests"
		);
		assert_eq!(
			git_stdout(&reused_spec.path, &["config", "--local", "--get", "user.email"]),
			"updated@example.com"
		);
		assert_eq!(
			git_stdout(&reused_spec.path, &["config", "--local", "--get-all", "user.name"]),
			"Updated Tests"
		);
		assert_eq!(
			git_stdout(&reused_spec.path, &["remote", "get-url", "origin"]),
			fs::canonicalize(&second_remote)
				.expect("second remote should canonicalize")
				.display()
				.to_string()
		);
	}

	#[test]
	fn rejects_reused_workspace_with_external_git_metadata() {
		let (_temp_dir, repo_root) = init_repo();
		let workspace_root = repo_root.join(".workspaces");
		let workspace_path = workspace_root.join("PUB-101");
		let external_git_dir = repo_root.join(".workspace-admin/PUB-101.git");

		fs::create_dir_all(&workspace_root).expect("workspace root should exist");
		fs::create_dir_all(external_git_dir.parent().unwrap())
			.expect("external git dir parent should exist");

		run_git(
			&repo_root,
			&[
				"clone",
				"--quiet",
				"--no-checkout",
				"--separate-git-dir",
				external_git_dir.to_str().unwrap(),
				".",
				workspace_path.to_str().unwrap(),
			],
		);

		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);
		let error = manager
			.ensure_workspace("PUB-101", false)
			.expect_err("non-self-contained workspace should be rejected");

		assert!(error.to_string().contains("is not self-contained: git "));
	}

	#[test]
	fn removes_clone_backed_workspace_path() {
		let (_temp_dir, repo_root) = init_repo();
		let workspace_root = repo_root.join(".workspaces");
		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);
		let spec = manager.ensure_workspace("PUB-101", false).expect("workspace should exist");

		assert!(manager.remove_workspace_path(&spec.path).expect("workspace should remove"));
		assert!(!spec.path.exists());
	}

	#[test]
	fn rejects_workspace_removal_when_path_escapes_root_via_parent_components() {
		let (_temp_dir, repo_root) = init_repo();
		let workspace_root = repo_root.join(".workspaces");
		let escaped_target = repo_root.join("outside").join("PUB-101");

		fs::create_dir_all(&workspace_root).expect("workspace root should exist");
		fs::create_dir_all(&escaped_target).expect("escaped target should exist");

		let manager = WorkspaceManager::new("pubfi", &repo_root, &workspace_root);
		let escaped_path = workspace_root.join("../outside/PUB-101");
		let error = manager
			.remove_workspace_path(&escaped_path)
			.expect_err("escaped workspace path should be rejected");

		assert!(error.to_string().contains("outside workspace_root"));
		assert!(escaped_target.exists());
	}
}
