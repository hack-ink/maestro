//! Thin in-memory runtime state for active Maestro execution.

#[cfg(unix)] use std::os::fd::{AsRawFd, FromRawFd};
use std::{
	cmp::Ordering,
	collections::{HashMap, HashSet},
	fs::{self, File, OpenOptions, TryLockError},
	io::{Error, ErrorKind, Read as _, Seek as _, SeekFrom, Write as _},
	path::{Path, PathBuf},
	process,
	sync::{Mutex, MutexGuard},
};

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::prelude::{Result, eyre};

pub(crate) const RUN_ACTIVITY_MARKER_FILE: &str = ".maestro-run-activity";

const DISPATCH_SLOT_LOCK_FILE_PREFIX: &str = ".maestro-dispatch-slot";
const ISSUE_CLAIM_LOCK_FILE_PREFIX: &str = ".maestro-issue-claim";
const STATE_GATE_LOCK_FILE_PREFIX: &str = ".maestro-state-gate";

/// Local runtime store for leases, attempts, workspaces, and protocol events.
#[derive(Default)]
pub struct StateStore {
	inner: Mutex<StateData>,
}
impl StateStore {
	/// Open a process-local runtime store.
	pub fn open(_path: impl AsRef<Path>) -> Result<Self> {
		Ok(Self::default())
	}

	/// Open an in-memory runtime store for tests and probes.
	pub fn open_in_memory() -> Result<Self> {
		Ok(Self::default())
	}

	/// Configure the shared cross-process dispatch-slot root for one project.
	pub fn configure_dispatch_slot_root(
		&self,
		project_id: &str,
		workspace_root: impl AsRef<Path>,
		slot_limit: u32,
	) -> Result<()> {
		self.configure_dispatch_slot_policy(project_id, workspace_root, slot_limit, &HashMap::new())
	}

	/// Configure the shared dispatch-slot root plus per-state limits for one project.
	pub fn configure_dispatch_slot_policy(
		&self,
		project_id: &str,
		workspace_root: impl AsRef<Path>,
		slot_limit: u32,
		state_slot_limits: &HashMap<String, u32>,
	) -> Result<()> {
		let mut state = self.lock()?;

		state.dispatch_slot_configs.insert(
			project_id.to_owned(),
			DispatchSlotConfig {
				root: workspace_root.as_ref().to_path_buf(),
				slot_limit: usize::try_from(slot_limit)
					.map_err(|_error| eyre::eyre!("dispatch slot limit overflowed usize"))?,
				state_slot_limits: state_slot_limits
					.iter()
					.map(|(state_name, limit)| {
						Ok((
							state_name.clone(),
							usize::try_from(*limit).map_err(|_error| {
								eyre::eyre!("state dispatch slot limit overflowed usize")
							})?,
						))
					})
					.collect::<Result<HashMap<_, _>>>()?,
			},
		);

		Ok(())
	}

	/// Create or replace the active lease for one issue.
	pub fn upsert_lease(
		&self,
		project_id: &str,
		issue_id: &str,
		run_id: &str,
		issue_state: &str,
	) -> Result<()> {
		let mut state = self.lock()?;

		state.leases.insert(
			issue_id.to_owned(),
			IssueLease {
				project_id: project_id.to_owned(),
				issue_id: issue_id.to_owned(),
				run_id: run_id.to_owned(),
				issue_state: issue_state.to_owned(),
			},
		);

		Ok(())
	}

	/// Try to acquire one issue claim plus one shared dispatch slot for one issue.
	pub fn try_acquire_lease(
		&self,
		project_id: &str,
		issue_id: &str,
		run_id: &str,
		issue_state: &str,
	) -> Result<bool> {
		let mut state = self.lock()?;

		if state.leases.values().any(|lease| lease.issue_id == issue_id) {
			return Ok(false);
		}

		if let Some(dispatch_slot_config) = state.dispatch_slot_configs.get(project_id).cloned() {
			fs::create_dir_all(&dispatch_slot_config.root)?;

			let issue_claim_lock_file = OpenOptions::new()
				.read(true)
				.write(true)
				.create(true)
				.truncate(false)
				.open(issue_claim_lock_path(&dispatch_slot_config.root, issue_id))?;

			match issue_claim_lock_file.try_lock() {
				Ok(()) => {},
				Err(TryLockError::WouldBlock) => return Ok(false),
				Err(TryLockError::Error(error)) => return Err(error.into()),
			}

			let mut issue_claim_guard =
				IssueClaimGuard { lock_file: issue_claim_lock_file, handoff_cloned: false };

			write_issue_claim_record(
				&mut issue_claim_guard.lock_file,
				project_id,
				issue_id,
				run_id,
				issue_state,
			)?;

			let state_gate_guard = acquire_state_gate_guard(
				&state,
				&dispatch_slot_config,
				project_id,
				issue_id,
				issue_state,
			)?;

			if state_gate_guard.blocked {
				issue_claim_guard.unlock()?;

				return Ok(false);
			}

			let held_slot_indexes = state
				.dispatch_slot_guards
				.values()
				.filter(|guard| guard.project_id == project_id)
				.map(|guard| guard.slot_index)
				.collect::<HashSet<_>>();
			let mut acquired_guard = None;

			for slot_index in 0..dispatch_slot_config.slot_limit {
				if held_slot_indexes.contains(&slot_index) {
					continue;
				}

				let lock_file = OpenOptions::new()
					.read(true)
					.write(true)
					.create(true)
					.truncate(false)
					.open(dispatch_slot_lock_path(&dispatch_slot_config.root, slot_index))?;

				match lock_file.try_lock() {
					Ok(()) => {
						acquired_guard = Some(DispatchSlotGuard {
							project_id: project_id.to_owned(),
							slot_index,
							lock_file,
							handoff_cloned: false,
						});

						break;
					},
					Err(TryLockError::WouldBlock) => continue,
					Err(TryLockError::Error(error)) => return Err(error.into()),
				}
			}

			let Some(dispatch_slot_guard) = acquired_guard else {
				issue_claim_guard.unlock()?;

				return Ok(false);
			};

			state.issue_claim_guards.insert(issue_id.to_owned(), issue_claim_guard);
			state.dispatch_slot_guards.insert(issue_id.to_owned(), dispatch_slot_guard);
		}

		state.leases.insert(
			issue_id.to_owned(),
			IssueLease {
				project_id: project_id.to_owned(),
				issue_id: issue_id.to_owned(),
				run_id: run_id.to_owned(),
				issue_state: issue_state.to_owned(),
			},
		);

		Ok(true)
	}

	/// Read the active lease for one issue.
	pub fn lease_for_issue(&self, issue_id: &str) -> Result<Option<IssueLease>> {
		let state = self.lock()?;

		Ok(state.leases.get(issue_id).cloned())
	}

	/// List all active leases.
	pub fn list_leases(&self, project_id: &str) -> Result<Vec<IssueLease>> {
		let state = self.lock()?;
		let mut leases = state
			.leases
			.values()
			.filter(|lease| lease.project_id == project_id)
			.cloned()
			.collect::<Vec<_>>();

		leases.sort_by(|left, right| left.issue_id.cmp(&right.issue_id));

		Ok(leases)
	}

	/// List all active shared leases by combining local claims with other processes' issue claims.
	pub fn list_active_shared_leases(&self, project_id: &str) -> Result<Vec<IssueLease>> {
		let (mut leases_by_issue, dispatch_slot_config) = {
			let state = self.lock()?;
			let leases = state
				.leases
				.values()
				.filter(|lease| lease.project_id == project_id)
				.cloned()
				.map(|lease| (lease.issue_id.clone(), lease))
				.collect::<HashMap<_, _>>();

			(leases, state.dispatch_slot_configs.get(project_id).cloned())
		};
		let Some(dispatch_slot_config) = dispatch_slot_config else {
			let mut leases = leases_by_issue.into_values().collect::<Vec<_>>();

			leases.sort_by(|left, right| left.issue_id.cmp(&right.issue_id));

			return Ok(leases);
		};
		let read_dir = match fs::read_dir(&dispatch_slot_config.root) {
			Ok(read_dir) => read_dir,
			Err(error) if error.kind() == ErrorKind::NotFound => {
				let mut leases = leases_by_issue.into_values().collect::<Vec<_>>();

				leases.sort_by(|left, right| left.issue_id.cmp(&right.issue_id));

				return Ok(leases);
			},
			Err(error) => return Err(error.into()),
		};

		for entry in read_dir {
			let entry = entry?;
			let path = entry.path();
			let Some(issue_id) = issue_claim_id_from_path(&path) else {
				continue;
			};

			if leases_by_issue.contains_key(&issue_id) {
				continue;
			}

			let claim_lock_file = match OpenOptions::new()
				.read(true)
				.write(true)
				.create(false)
				.truncate(false)
				.open(&path)
			{
				Ok(file) => file,
				Err(error) if error.kind() == ErrorKind::NotFound => continue,
				Err(error) => return Err(error.into()),
			};

			match claim_lock_file.try_lock() {
				Ok(()) => claim_lock_file.unlock()?,
				Err(TryLockError::WouldBlock) => {
					if let Some(lease) = read_issue_claim_record(&path)?
						&& lease.project_id == project_id
					{
						leases_by_issue.insert(issue_id, lease);
					}
				},
				Err(TryLockError::Error(error)) => return Err(error.into()),
			}
		}

		let mut leases = leases_by_issue.into_values().collect::<Vec<_>>();

		leases.sort_by(|left, right| left.issue_id.cmp(&right.issue_id));

		Ok(leases)
	}

	/// Report whether one issue is actively claimed by this or another process.
	pub fn issue_has_active_shared_claim(&self, project_id: &str, issue_id: &str) -> Result<bool> {
		let state = self.lock()?;

		if state.leases.contains_key(issue_id) {
			return Ok(true);
		}

		let Some(dispatch_slot_config) = state.dispatch_slot_configs.get(project_id).cloned()
		else {
			return Ok(false);
		};

		drop(state);

		let path = issue_claim_lock_path(&dispatch_slot_config.root, issue_id);
		let claim_lock_file = match OpenOptions::new()
			.read(true)
			.write(true)
			.create(false)
			.truncate(false)
			.open(path)
		{
			Ok(file) => file,
			Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
			Err(error) => return Err(error.into()),
		};

		match claim_lock_file.try_lock() {
			Ok(()) => {
				claim_lock_file.unlock()?;

				Ok(false)
			},
			Err(TryLockError::WouldBlock) => Ok(true),
			Err(TryLockError::Error(error)) => Err(error.into()),
		}
	}

	/// Remove the active lease for one issue.
	pub fn clear_lease(&self, issue_id: &str) -> Result<()> {
		let mut state = self.lock()?;

		if state.leases.remove(issue_id).is_none() {
			return Ok(());
		}

		if let Some(guard) = state.issue_claim_guards.remove(issue_id) {
			guard.release_for_clear()?;
		}
		if let Some(guard) = state.dispatch_slot_guards.remove(issue_id) {
			guard.release_for_clear()?;
		}

		Ok(())
	}

	/// Drop the current process-local dispatch-slot guard while keeping the local lease record.
	pub fn release_dispatch_slot(&self, issue_id: &str) -> Result<()> {
		let mut state = self.lock()?;

		state.dispatch_slot_guards.remove(issue_id);

		Ok(())
	}

	/// Duplicate the held dispatch-slot lock so a spawned child can inherit it across exec.
	#[cfg(unix)]
	pub fn clone_issue_claim_for_child(&self, issue_id: &str) -> Result<File> {
		let mut state = self.lock()?;
		let guard = state
			.issue_claim_guards
			.get_mut(issue_id)
			.ok_or_else(|| eyre::eyre!("issue `{issue_id}` does not hold an issue-claim guard"))?;

		guard.handoff_cloned = true;

		let child_lock = guard.lock_file.try_clone()?;

		clear_close_on_exec(&child_lock)?;

		Ok(child_lock)
	}

	/// Duplicate the held dispatch-slot lock so a spawned child can inherit it across exec.
	#[cfg(unix)]
	pub fn clone_dispatch_slot_for_child(&self, issue_id: &str) -> Result<(File, usize)> {
		let mut state = self.lock()?;
		let guard = state
			.dispatch_slot_guards
			.get_mut(issue_id)
			.ok_or_else(|| eyre::eyre!("issue `{issue_id}` does not hold a dispatch-slot guard"))?;

		guard.handoff_cloned = true;

		let child_lock = guard.lock_file.try_clone()?;

		clear_close_on_exec(&child_lock)?;

		Ok((child_lock, guard.slot_index))
	}

	/// Adopt an inherited dispatch-slot fd and local lease for a daemon child process.
	#[cfg(unix)]
	pub fn adopt_preacquired_lease(
		&self,
		project_id: &str,
		issue_id: &str,
		run_id: &str,
		issue_state: &str,
		guards: PreacquiredLeaseGuards,
	) -> Result<()> {
		let issue_claim_lock_file = unsafe { File::from_raw_fd(guards.issue_claim_fd) };
		let lock_file = unsafe { File::from_raw_fd(guards.dispatch_slot_fd) };
		let mut state = self.lock()?;

		state.issue_claim_guards.insert(
			issue_id.to_owned(),
			IssueClaimGuard { lock_file: issue_claim_lock_file, handoff_cloned: true },
		);
		state.dispatch_slot_guards.insert(
			issue_id.to_owned(),
			DispatchSlotGuard {
				project_id: project_id.to_owned(),
				slot_index: guards.dispatch_slot_index,
				lock_file,
				handoff_cloned: true,
			},
		);
		state.leases.insert(
			issue_id.to_owned(),
			IssueLease {
				project_id: project_id.to_owned(),
				issue_id: issue_id.to_owned(),
				run_id: run_id.to_owned(),
				issue_state: issue_state.to_owned(),
			},
		);

		Ok(())
	}

	/// Insert or update a run attempt record.
	pub fn record_run_attempt(
		&self,
		run_id: &str,
		issue_id: &str,
		attempt_number: i64,
		status: &str,
	) -> Result<()> {
		let now = timestamp_parts();
		let mut state = self.lock()?;

		match state.run_attempts.get_mut(run_id) {
			Some(existing) => {
				existing.issue_id = issue_id.to_owned();
				existing.attempt_number = attempt_number;
				existing.status = status.to_owned();
				existing.updated_at = now.text.clone();
				existing.updated_at_unix = now.unix;
			},
			None => {
				state.run_attempts.insert(
					run_id.to_owned(),
					RunAttemptRecord {
						run_id: run_id.to_owned(),
						issue_id: issue_id.to_owned(),
						attempt_number,
						status: status.to_owned(),
						thread_id: None,
						updated_at: now.text,
						updated_at_unix: now.unix,
					},
				);
			},
		}

		Ok(())
	}

	/// Compute the next attempt number for one issue.
	pub fn next_attempt_number(&self, issue_id: &str) -> Result<i64> {
		let state = self.lock()?;
		let next_attempt = state
			.run_attempts
			.values()
			.filter(|attempt| attempt.issue_id == issue_id)
			.map(|attempt| attempt.attempt_number)
			.max()
			.unwrap_or(0)
			+ 1;

		Ok(next_attempt)
	}

	/// Count attempts that consume the retry budget for one issue.
	pub fn retry_budget_attempt_count(&self, issue_id: &str) -> Result<i64> {
		let state = self.lock()?;
		let retry_budget_attempts = state
			.run_attempts
			.values()
			.filter(|attempt| {
				attempt.issue_id == issue_id
					&& matches!(
						attempt.status.as_str(),
						"failed" | "interrupted" | "terminal_guarded"
					)
			})
			.count() as i64;

		Ok(retry_budget_attempts)
	}

	/// Attach the active thread identifier to a run attempt.
	pub fn update_run_thread(&self, run_id: &str, thread_id: &str) -> Result<()> {
		let now = timestamp_parts();
		let mut state = self.lock()?;

		if let Some(attempt) = state.run_attempts.get_mut(run_id) {
			attempt.thread_id = Some(thread_id.to_owned());
			attempt.updated_at = now.text;
			attempt.updated_at_unix = now.unix;
		}

		Ok(())
	}

	/// Update the status for one run attempt.
	pub fn update_run_status(&self, run_id: &str, status: &str) -> Result<()> {
		let now = timestamp_parts();
		let mut state = self.lock()?;

		if let Some(attempt) = state.run_attempts.get_mut(run_id) {
			attempt.status = status.to_owned();
			attempt.updated_at = now.text;
			attempt.updated_at_unix = now.unix;
		}

		Ok(())
	}

	/// Read one run attempt.
	pub fn run_attempt(&self, run_id: &str) -> Result<Option<RunAttempt>> {
		let state = self.lock()?;

		Ok(state.run_attempts.get(run_id).map(RunAttemptRecord::as_public))
	}

	/// Read one run attempt by issue and attempt number.
	pub fn run_attempt_for_issue_attempt(
		&self,
		issue_id: &str,
		attempt_number: i64,
	) -> Result<Option<RunAttempt>> {
		let state = self.lock()?;
		let attempt = state
			.run_attempts
			.values()
			.filter(|attempt| {
				attempt.issue_id == issue_id && attempt.attempt_number == attempt_number
			})
			.max_by(|left, right| compare_attempt_records(left, right))
			.map(RunAttemptRecord::as_public);

		Ok(attempt)
	}

	/// Read the latest run attempt for one issue.
	pub fn latest_run_attempt_for_issue(&self, issue_id: &str) -> Result<Option<RunAttempt>> {
		let state = self.lock()?;
		let attempt = state
			.run_attempts
			.values()
			.filter(|attempt| attempt.issue_id == issue_id)
			.max_by(|left, right| compare_attempt_records(left, right))
			.map(RunAttemptRecord::as_public);

		Ok(attempt)
	}

	/// List recent run attempts for one project, including lease and protocol summary fields.
	pub fn list_recent_runs(
		&self,
		project_id: &str,
		limit: usize,
	) -> Result<Vec<ProjectRunStatus>> {
		let state = self.lock()?;
		let mut runs = state
			.run_attempts
			.values()
			.filter_map(|attempt| state.project_run_status(project_id, attempt))
			.collect::<Vec<_>>();

		runs.sort_by(compare_project_run_status);
		runs.truncate(limit);

		Ok(runs)
	}

	/// List all active leased runs for one project without applying the recent-run limit.
	pub fn list_active_runs(&self, project_id: &str) -> Result<Vec<ProjectRunStatus>> {
		let state = self.lock()?;
		let mut runs = state
			.run_attempts
			.values()
			.filter_map(|attempt| {
				let status = state.project_run_status(project_id, attempt)?;

				status.active_lease.then_some(status)
			})
			.collect::<Vec<_>>();

		runs.sort_by(compare_project_run_status);

		Ok(runs)
	}

	/// Append one protocol event to the journal for a run.
	pub fn append_event(
		&self,
		run_id: &str,
		sequence_number: i64,
		event_type: &str,
		_payload: &str,
	) -> Result<()> {
		let mut state = self.lock()?;
		let events = state.events.entry(run_id.to_owned()).or_default();

		if events.iter().any(|event| event.sequence_number == sequence_number) {
			eyre::bail!(
				"Protocol event `{run_id}` sequence `{sequence_number}` already exists in the in-memory journal."
			);
		}

		let now = timestamp_parts();

		events.push(ProtocolEventRecord {
			sequence_number,
			event_type: event_type.to_owned(),
			created_at: now.text,
			created_at_unix: now.unix,
		});
		events.sort_by(|left, right| left.sequence_number.cmp(&right.sequence_number));

		Ok(())
	}

	/// Count protocol journal records for one run.
	pub fn event_count(&self, run_id: &str) -> Result<i64> {
		let state = self.lock()?;

		Ok(state.events.get(run_id).map_or(0, |events| events.len() as i64))
	}

	/// Read the latest recorded activity timestamp for one run as a Unix epoch.
	pub fn last_run_activity_unix_epoch(&self, run_id: &str) -> Result<Option<i64>> {
		let state = self.lock()?;
		let last_activity = state.run_attempts.get(run_id).map(|attempt| attempt.updated_at_unix);
		let last_event = state
			.events
			.get(run_id)
			.and_then(|events| events.iter().map(|event| event.created_at_unix).max());

		Ok(match (last_activity, last_event) {
			(Some(run_activity), Some(event_activity)) => Some(run_activity.max(event_activity)),
			(Some(run_activity), None) => Some(run_activity),
			(None, Some(event_activity)) => Some(event_activity),
			(None, None) => None,
		})
	}

	/// Read the latest recorded protocol-event timestamp for one run as a Unix epoch.
	pub fn last_protocol_activity_unix_epoch(&self, run_id: &str) -> Result<Option<i64>> {
		let state = self.lock()?;

		Ok(state
			.events
			.get(run_id)
			.and_then(|events| events.iter().map(|event| event.created_at_unix).max()))
	}

	/// Create or replace the workspace mapping for one issue.
	pub fn upsert_workspace(
		&self,
		project_id: &str,
		issue_id: &str,
		branch_name: &str,
		workspace_path: &str,
	) -> Result<()> {
		let mut state = self.lock()?;

		state.workspaces.insert(
			issue_id.to_owned(),
			WorkspaceMappingRecord {
				project_id: project_id.to_owned(),
				issue_id: issue_id.to_owned(),
				branch_name: branch_name.to_owned(),
				workspace_path: PathBuf::from(workspace_path),
			},
		);

		Ok(())
	}

	/// Read the workspace mapping for one issue.
	pub fn workspace_for_issue(&self, issue_id: &str) -> Result<Option<WorkspaceMapping>> {
		let state = self.lock()?;

		Ok(state.workspaces.get(issue_id).map(WorkspaceMappingRecord::as_public))
	}

	/// List all known workspace mappings.
	pub fn list_workspaces(&self, project_id: &str) -> Result<Vec<WorkspaceMapping>> {
		let state = self.lock()?;
		let mut mappings = state
			.workspaces
			.values()
			.filter(|mapping| mapping.project_id == project_id)
			.map(WorkspaceMappingRecord::as_public)
			.collect::<Vec<_>>();

		mappings.sort_by(|left, right| left.issue_id.cmp(&right.issue_id));

		Ok(mappings)
	}

	/// Remove the workspace mapping for one issue.
	pub fn clear_workspace(&self, issue_id: &str) -> Result<()> {
		let mut state = self.lock()?;

		state.workspaces.remove(issue_id);

		Ok(())
	}

	fn lock(&self) -> Result<MutexGuard<'_, StateData>> {
		self.inner.lock().map_err(|_| eyre::eyre!("StateStore mutex is poisoned."))
	}
}

/// Active lease for one issue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssueLease {
	project_id: String,
	issue_id: String,
	run_id: String,
	issue_state: String,
}
impl IssueLease {
	/// Local project identifier owning this lease.
	pub fn project_id(&self) -> &str {
		&self.project_id
	}

	/// Issue identifier owning the lease.
	pub fn issue_id(&self) -> &str {
		&self.issue_id
	}

	/// Run identifier holding the lease.
	pub fn run_id(&self) -> &str {
		&self.run_id
	}

	/// Tracker state representing the dispatched run.
	pub fn issue_state(&self) -> &str {
		&self.issue_state
	}
}

/// Persistent run attempt metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunAttempt {
	run_id: String,
	issue_id: String,
	attempt_number: i64,
	status: String,
	thread_id: Option<String>,
}
impl RunAttempt {
	/// Stable run identifier.
	pub fn run_id(&self) -> &str {
		&self.run_id
	}

	/// Issue identifier for the run.
	pub fn issue_id(&self) -> &str {
		&self.issue_id
	}

	/// Attempt number for this run.
	pub fn attempt_number(&self) -> i64 {
		self.attempt_number
	}

	/// Current local status for the run.
	pub fn status(&self) -> &str {
		&self.status
	}

	/// Thread identifier returned by `app-server`, when known.
	pub fn thread_id(&self) -> Option<&str> {
		self.thread_id.as_deref()
	}
}

/// Project-scoped operator view of one run attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectRunStatus {
	run_id: String,
	issue_id: String,
	attempt_number: i64,
	status: String,
	thread_id: Option<String>,
	updated_at: String,
	branch_name: Option<String>,
	workspace_path: Option<PathBuf>,
	active_lease: bool,
	event_count: i64,
	last_event_type: Option<String>,
	last_event_at: Option<String>,
}
impl ProjectRunStatus {
	/// Stable run identifier.
	pub fn run_id(&self) -> &str {
		&self.run_id
	}

	/// Issue identifier for the run.
	pub fn issue_id(&self) -> &str {
		&self.issue_id
	}

	/// Attempt number for this run.
	pub fn attempt_number(&self) -> i64 {
		self.attempt_number
	}

	/// Current local status for the run.
	pub fn status(&self) -> &str {
		&self.status
	}

	/// Thread identifier returned by `app-server`, when known.
	pub fn thread_id(&self) -> Option<&str> {
		self.thread_id.as_deref()
	}

	/// Timestamp of the latest run-attempt status update.
	pub fn updated_at(&self) -> &str {
		&self.updated_at
	}

	/// Branch name for the retained lane, when known.
	pub fn branch_name(&self) -> Option<&str> {
		self.branch_name.as_deref()
	}

	/// Filesystem path to the retained workspace, when known.
	pub fn workspace_path(&self) -> Option<&Path> {
		self.workspace_path.as_deref()
	}

	/// Whether this run still holds the active local lease.
	pub fn active_lease(&self) -> bool {
		self.active_lease
	}

	/// Number of recorded protocol events for the run.
	pub fn event_count(&self) -> i64 {
		self.event_count
	}

	/// Latest recorded protocol event type, when one exists.
	pub fn last_event_type(&self) -> Option<&str> {
		self.last_event_type.as_deref()
	}

	/// Timestamp of the latest recorded protocol event, when one exists.
	pub fn last_event_at(&self) -> Option<&str> {
		self.last_event_at.as_deref()
	}
}

/// Workspace mapping for one issue lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceMapping {
	project_id: String,
	issue_id: String,
	branch_name: String,
	workspace_path: PathBuf,
}
impl WorkspaceMapping {
	/// Local project identifier owning this lane.
	pub fn project_id(&self) -> &str {
		&self.project_id
	}

	/// Issue identifier for this lane.
	pub fn issue_id(&self) -> &str {
		&self.issue_id
	}

	/// Branch name used for the lane.
	pub fn branch_name(&self) -> &str {
		&self.branch_name
	}

	/// Filesystem path to the workspace checkout.
	pub fn workspace_path(&self) -> &Path {
		&self.workspace_path
	}
}

/// Unix file-descriptor handoff for a daemon-planned lease adopted by a child process.
pub struct PreacquiredLeaseGuards {
	/// The inherited issue-claim lock fd that keeps one issue single-owned across processes.
	pub issue_claim_fd: i32,
	/// The inherited dispatch-slot lock fd that keeps one shared capacity slot occupied.
	pub dispatch_slot_fd: i32,
	/// The inherited shared dispatch-slot index used for local guard bookkeeping.
	pub dispatch_slot_index: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunActivityMarker {
	run_id: String,
	attempt_number: i64,
	process_id: Option<u32>,
	last_activity_unix_epoch: Option<i64>,
	last_protocol_activity_unix_epoch: Option<i64>,
	retry_budget_attempt_count: Option<i64>,
}
impl RunActivityMarker {
	pub(crate) fn run_id(&self) -> &str {
		&self.run_id
	}

	pub(crate) fn attempt_number(&self) -> i64 {
		self.attempt_number
	}

	pub(crate) fn process_id(&self) -> Option<u32> {
		self.process_id
	}

	pub(crate) fn last_activity_unix_epoch(&self) -> Option<i64> {
		self.last_activity_unix_epoch
	}
}

#[derive(Clone)]
struct DispatchSlotConfig {
	root: PathBuf,
	slot_limit: usize,
	state_slot_limits: HashMap<String, usize>,
}

struct IssueClaimGuard {
	lock_file: File,
	handoff_cloned: bool,
}
impl IssueClaimGuard {
	fn unlock(self) -> Result<()> {
		self.lock_file.unlock()?;

		Ok(())
	}

	fn release_for_clear(self) -> Result<()> {
		if self.handoff_cloned {
			return Ok(());
		}

		self.unlock()
	}
}

struct DispatchSlotGuard {
	project_id: String,
	slot_index: usize,
	lock_file: File,
	handoff_cloned: bool,
}
impl DispatchSlotGuard {
	fn release_for_clear(self) -> Result<()> {
		if self.handoff_cloned {
			return Ok(());
		}

		self.lock_file.unlock()?;

		Ok(())
	}
}

#[derive(Default)]
struct StateData {
	leases: HashMap<String, IssueLease>,
	run_attempts: HashMap<String, RunAttemptRecord>,
	events: HashMap<String, Vec<ProtocolEventRecord>>,
	workspaces: HashMap<String, WorkspaceMappingRecord>,
	dispatch_slot_configs: HashMap<String, DispatchSlotConfig>,
	issue_claim_guards: HashMap<String, IssueClaimGuard>,
	dispatch_slot_guards: HashMap<String, DispatchSlotGuard>,
}
impl StateData {
	fn project_run_status(
		&self,
		project_id: &str,
		attempt: &RunAttemptRecord,
	) -> Option<ProjectRunStatus> {
		let workspace = self.workspaces.get(&attempt.issue_id);
		let active_lease = self
			.leases
			.get(&attempt.issue_id)
			.is_some_and(|lease| lease.project_id == project_id && lease.run_id == attempt.run_id);
		let in_project =
			workspace.is_some_and(|mapping| mapping.project_id == project_id) || active_lease;

		if !in_project {
			return None;
		}

		let (event_count, last_event_type, last_event_at) = match self.events.get(&attempt.run_id) {
			Some(events) => {
				let last = events
					.iter()
					.max_by(|left, right| left.sequence_number.cmp(&right.sequence_number));

				(
					events.len() as i64,
					last.map(|event| event.event_type.clone()),
					last.map(|event| event.created_at.clone()),
				)
			},
			None => (0, None, None),
		};

		Some(ProjectRunStatus {
			run_id: attempt.run_id.clone(),
			issue_id: attempt.issue_id.clone(),
			attempt_number: attempt.attempt_number,
			status: attempt.status.clone(),
			thread_id: attempt.thread_id.clone(),
			updated_at: attempt.updated_at.clone(),
			branch_name: workspace.map(|mapping| mapping.branch_name.clone()),
			workspace_path: workspace.map(|mapping| mapping.workspace_path.clone()),
			active_lease,
			event_count,
			last_event_type,
			last_event_at,
		})
	}
}

struct TimestampParts {
	text: String,
	unix: i64,
}

#[derive(Clone, Debug)]
struct RunAttemptRecord {
	run_id: String,
	issue_id: String,
	attempt_number: i64,
	status: String,
	thread_id: Option<String>,
	updated_at: String,
	updated_at_unix: i64,
}
impl RunAttemptRecord {
	fn as_public(&self) -> RunAttempt {
		RunAttempt {
			run_id: self.run_id.clone(),
			issue_id: self.issue_id.clone(),
			attempt_number: self.attempt_number,
			status: self.status.clone(),
			thread_id: self.thread_id.clone(),
		}
	}
}

#[derive(Clone, Debug)]
struct ProtocolEventRecord {
	sequence_number: i64,
	event_type: String,
	created_at: String,
	created_at_unix: i64,
}

#[derive(Clone, Debug)]
struct WorkspaceMappingRecord {
	project_id: String,
	issue_id: String,
	branch_name: String,
	workspace_path: PathBuf,
}
impl WorkspaceMappingRecord {
	fn as_public(&self) -> WorkspaceMapping {
		WorkspaceMapping {
			project_id: self.project_id.clone(),
			issue_id: self.issue_id.clone(),
			branch_name: self.branch_name.clone(),
			workspace_path: self.workspace_path.clone(),
		}
	}
}

#[derive(Default)]
struct RunActivityMarkerRecord {
	run_id: Option<String>,
	attempt_number: Option<i64>,
	process_id: Option<u32>,
	last_activity_unix_epoch: Option<i64>,
	last_protocol_activity_unix_epoch: Option<i64>,
	retry_budget_attempt_count: Option<i64>,
}

struct StateGateGuard {
	blocked: bool,
	_guard: Option<File>,
}

pub(crate) fn write_run_activity_marker(
	workspace_path: &Path,
	run_id: &str,
	attempt_number: i64,
) -> Result<()> {
	write_run_activity_marker_for_process(workspace_path, run_id, attempt_number, process::id())
}

pub(crate) fn write_run_activity_marker_for_process(
	workspace_path: &Path,
	run_id: &str,
	attempt_number: i64,
	process_id: u32,
) -> Result<()> {
	write_run_activity_marker_at(
		workspace_path,
		run_id,
		attempt_number,
		process_id,
		OffsetDateTime::now_utc().unix_timestamp(),
		None,
	)
}

pub(crate) fn write_run_protocol_activity_marker(
	workspace_path: &Path,
	run_id: &str,
	attempt_number: i64,
) -> Result<()> {
	let now = OffsetDateTime::now_utc().unix_timestamp();

	write_run_activity_marker_at(
		workspace_path,
		run_id,
		attempt_number,
		process::id(),
		now,
		Some(now),
	)
}

pub(crate) fn read_run_activity_marker(
	workspace_path: &Path,
	run_id: &str,
	attempt_number: i64,
) -> Result<Option<i64>> {
	let marker = read_run_activity_marker_record(workspace_path)?.filter(|marker| {
		marker.run_id.as_deref() == Some(run_id) && marker.attempt_number == Some(attempt_number)
	});

	Ok(marker.and_then(|marker| marker.last_activity_unix_epoch))
}

pub(crate) fn read_run_protocol_activity_marker(
	workspace_path: &Path,
	run_id: &str,
	attempt_number: i64,
) -> Result<Option<i64>> {
	let marker = read_run_activity_marker_record(workspace_path)?.filter(|marker| {
		marker.run_id.as_deref() == Some(run_id) && marker.attempt_number == Some(attempt_number)
	});

	Ok(marker.and_then(|marker| marker.last_protocol_activity_unix_epoch))
}

pub(crate) fn write_run_retry_budget_attempt_count(
	workspace_path: &Path,
	run_id: &str,
	attempt_number: i64,
	retry_budget_attempt_count: i64,
) -> Result<()> {
	fs::create_dir_all(workspace_path)?;

	let existing_marker = read_run_activity_marker_record(workspace_path)?;
	let last_activity_unix_epoch =
		existing_marker.as_ref().and_then(|marker| marker.last_activity_unix_epoch);
	let last_protocol_activity_unix_epoch =
		existing_marker.as_ref().and_then(|marker| marker.last_protocol_activity_unix_epoch);
	let mut marker_body = format!(
		"run_id={run_id}\nattempt_number={attempt_number}\nprocess_id={}\nretry_budget_attempt_count={retry_budget_attempt_count}\n",
		existing_marker.as_ref().and_then(|marker| marker.process_id).unwrap_or_else(process::id)
	);

	if let Some(last_activity_unix_epoch) = last_activity_unix_epoch {
		marker_body
			.push_str(format!("last_activity_unix_epoch={last_activity_unix_epoch}\n").as_str());
	}
	if let Some(last_protocol_activity_unix_epoch) = last_protocol_activity_unix_epoch {
		marker_body.push_str(
			format!("last_protocol_activity_unix_epoch={last_protocol_activity_unix_epoch}\n")
				.as_str(),
		);
	}

	fs::write(workspace_path.join(RUN_ACTIVITY_MARKER_FILE), marker_body)?;

	Ok(())
}

pub(crate) fn read_run_retry_budget_attempt_count(workspace_path: &Path) -> Result<Option<i64>> {
	Ok(read_run_activity_marker_record(workspace_path)?
		.and_then(|marker| marker.retry_budget_attempt_count))
}

pub(crate) fn read_run_activity_marker_snapshot(
	workspace_path: &Path,
) -> Result<Option<RunActivityMarker>> {
	Ok(read_run_activity_marker_record(workspace_path)?.and_then(|marker| {
		Some(RunActivityMarker {
			run_id: marker.run_id?,
			attempt_number: marker.attempt_number?,
			process_id: marker.process_id,
			last_activity_unix_epoch: marker.last_activity_unix_epoch,
			last_protocol_activity_unix_epoch: marker.last_protocol_activity_unix_epoch,
			retry_budget_attempt_count: marker.retry_budget_attempt_count,
		})
	}))
}

fn dispatch_slot_lock_path(root: &Path, slot_index: usize) -> PathBuf {
	root.join(format!("{DISPATCH_SLOT_LOCK_FILE_PREFIX}.{slot_index}.lock"))
}

fn issue_claim_lock_path(root: &Path, issue_id: &str) -> PathBuf {
	root.join(format!("{ISSUE_CLAIM_LOCK_FILE_PREFIX}.{issue_id}.lock"))
}

fn state_gate_lock_path(root: &Path, state_name: &str) -> PathBuf {
	root.join(format!("{STATE_GATE_LOCK_FILE_PREFIX}.{}.lock", encode_lock_component(state_name)))
}

fn encode_lock_component(value: &str) -> String {
	const HEX: &[u8; 16] = b"0123456789abcdef";

	let mut encoded = String::with_capacity(value.len() * 2);

	for byte in value.as_bytes() {
		encoded.push(char::from(HEX[usize::from(byte >> 4)]));
		encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
	}

	encoded
}

fn issue_claim_id_from_path(path: &Path) -> Option<String> {
	let file_name = path.file_name()?.to_str()?;

	file_name
		.strip_prefix(&format!("{ISSUE_CLAIM_LOCK_FILE_PREFIX}."))
		.and_then(|suffix| suffix.strip_suffix(".lock"))
		.map(str::to_owned)
}

fn write_issue_claim_record(
	lock_file: &mut File,
	project_id: &str,
	issue_id: &str,
	run_id: &str,
	issue_state: &str,
) -> Result<()> {
	lock_file.set_len(0)?;
	lock_file.seek(SeekFrom::Start(0))?;

	write!(
		lock_file,
		"project_id={project_id}\nissue_id={issue_id}\nrun_id={run_id}\nissue_state={issue_state}\n"
	)?;

	lock_file.flush()?;

	Ok(())
}

fn read_issue_claim_record(path: &Path) -> Result<Option<IssueLease>> {
	let mut body = String::new();
	let mut file = File::open(path)?;

	file.read_to_string(&mut body)?;

	if body.trim().is_empty() {
		return Ok(None);
	}

	let mut project_id = None;
	let mut issue_id = None;
	let mut run_id = None;
	let mut issue_state = None;

	for line in body.lines().filter(|line| !line.trim().is_empty()) {
		let (key, value) = line
			.split_once('=')
			.ok_or_else(|| eyre::eyre!("issue claim record `{}` is malformed", path.display()))?;

		match key {
			"project_id" => project_id = Some(value.to_owned()),
			"issue_id" => issue_id = Some(value.to_owned()),
			"run_id" => run_id = Some(value.to_owned()),
			"issue_state" => issue_state = Some(value.to_owned()),
			_ => {},
		}
	}

	let Some(project_id) = project_id else {
		return Err(eyre::eyre!("issue claim record `{}` is missing project_id", path.display()));
	};
	let Some(issue_id) = issue_id else {
		return Err(eyre::eyre!("issue claim record `{}` is missing issue_id", path.display()));
	};
	let Some(run_id) = run_id else {
		return Err(eyre::eyre!("issue claim record `{}` is missing run_id", path.display()));
	};
	let Some(issue_state) = issue_state else {
		return Err(eyre::eyre!("issue claim record `{}` is missing issue_state", path.display()));
	};

	Ok(Some(IssueLease { project_id, issue_id, run_id, issue_state }))
}

fn count_shared_state_occupancy(
	state: &StateData,
	dispatch_slot_config: &DispatchSlotConfig,
	project_id: &str,
	issue_state: &str,
	excluded_issue_id: &str,
) -> Result<usize> {
	let mut seen_issue_ids = HashSet::from([excluded_issue_id.to_owned()]);
	let mut occupied = 0;

	for lease in state.leases.values() {
		if lease.project_id == project_id
			&& lease.issue_state == issue_state
			&& seen_issue_ids.insert(lease.issue_id.clone())
		{
			occupied += 1;
		}
	}

	let read_dir = match fs::read_dir(&dispatch_slot_config.root) {
		Ok(read_dir) => read_dir,
		Err(error) if error.kind() == ErrorKind::NotFound => return Ok(occupied),
		Err(error) => return Err(error.into()),
	};

	for entry in read_dir {
		let entry = entry?;
		let path = entry.path();
		let Some(issue_id) = issue_claim_id_from_path(&path) else {
			continue;
		};

		if !seen_issue_ids.insert(issue_id.clone()) {
			continue;
		}

		let claim_lock_file = match OpenOptions::new()
			.read(true)
			.write(true)
			.create(false)
			.truncate(false)
			.open(&path)
		{
			Ok(file) => file,
			Err(error) if error.kind() == ErrorKind::NotFound => continue,
			Err(error) => return Err(error.into()),
		};

		match claim_lock_file.try_lock() {
			Ok(()) => claim_lock_file.unlock()?,
			Err(TryLockError::WouldBlock) => {
				if let Some(lease) = read_issue_claim_record(&path)?
					&& lease.project_id == project_id
					&& lease.issue_state == issue_state
				{
					occupied += 1;
				}
			},
			Err(TryLockError::Error(error)) => return Err(error.into()),
		}
	}

	Ok(occupied)
}

fn acquire_state_gate_guard(
	state: &StateData,
	dispatch_slot_config: &DispatchSlotConfig,
	project_id: &str,
	issue_id: &str,
	issue_state: &str,
) -> Result<StateGateGuard> {
	let Some(limit) = dispatch_slot_config.state_slot_limits.get(issue_state).copied() else {
		return Ok(StateGateGuard { blocked: false, _guard: None });
	};
	let state_gate_path = state_gate_lock_path(&dispatch_slot_config.root, issue_state);
	let state_gate_lock_file = OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(false)
		.open(state_gate_path)?;

	state_gate_lock_file.lock()?;

	let occupied = count_shared_state_occupancy(
		state,
		dispatch_slot_config,
		project_id,
		issue_state,
		issue_id,
	)?;

	if occupied >= limit {
		return Ok(StateGateGuard { blocked: true, _guard: Some(state_gate_lock_file) });
	}

	Ok(StateGateGuard { blocked: false, _guard: Some(state_gate_lock_file) })
}

fn write_run_activity_marker_at(
	workspace_path: &Path,
	run_id: &str,
	attempt_number: i64,
	process_id: u32,
	last_activity_unix_epoch: i64,
	last_protocol_activity_unix_epoch: Option<i64>,
) -> Result<()> {
	let marker_path = workspace_path.join(RUN_ACTIVITY_MARKER_FILE);
	let preserved_protocol_activity =
		read_run_activity_marker_record(workspace_path)?.and_then(|marker| {
			(marker.run_id.as_deref() == Some(run_id)
				&& marker.attempt_number == Some(attempt_number))
			.then_some(marker.last_protocol_activity_unix_epoch)
			.flatten()
		});
	let preserved_retry_budget_attempt_count = read_run_activity_marker_record(workspace_path)?
		.and_then(|marker| marker.retry_budget_attempt_count);
	let last_protocol_activity_unix_epoch =
		last_protocol_activity_unix_epoch.or(preserved_protocol_activity);
	let mut marker_body = format!(
		"run_id={run_id}\nattempt_number={attempt_number}\nprocess_id={process_id}\nlast_activity_unix_epoch={last_activity_unix_epoch}\n"
	);

	if let Some(last_protocol_activity_unix_epoch) = last_protocol_activity_unix_epoch {
		marker_body.push_str(
			format!("last_protocol_activity_unix_epoch={last_protocol_activity_unix_epoch}\n")
				.as_str(),
		);
	}
	if let Some(retry_budget_attempt_count) = preserved_retry_budget_attempt_count {
		marker_body.push_str(
			format!("retry_budget_attempt_count={retry_budget_attempt_count}\n").as_str(),
		);
	}

	fs::write(marker_path, marker_body)?;

	Ok(())
}

fn read_run_activity_marker_record(
	workspace_path: &Path,
) -> Result<Option<RunActivityMarkerRecord>> {
	let marker_path = workspace_path.join(RUN_ACTIVITY_MARKER_FILE);
	let marker_body = match fs::read_to_string(&marker_path) {
		Ok(body) => body,
		Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
		Err(error) => return Err(error.into()),
	};
	let mut marker = RunActivityMarkerRecord::default();

	for line in marker_body.lines() {
		let Some((key, value)) = line.split_once('=') else {
			continue;
		};

		match key {
			"run_id" => marker.run_id = Some(value.to_owned()),
			"attempt_number" => marker.attempt_number = value.parse::<i64>().ok(),
			"process_id" => marker.process_id = value.parse::<u32>().ok(),
			"last_activity_unix_epoch" =>
				marker.last_activity_unix_epoch = value.parse::<i64>().ok(),
			"last_protocol_activity_unix_epoch" =>
				marker.last_protocol_activity_unix_epoch = value.parse::<i64>().ok(),
			"retry_budget_attempt_count" =>
				marker.retry_budget_attempt_count = value.parse::<i64>().ok(),
			_ => {},
		}
	}

	Ok(Some(marker))
}

fn timestamp_parts() -> TimestampParts {
	let now = OffsetDateTime::now_utc();

	TimestampParts {
		text: now.format(&Rfc3339).expect("timestamp formatting should succeed"),
		unix: now.unix_timestamp(),
	}
}

fn compare_attempt_records(left: &RunAttemptRecord, right: &RunAttemptRecord) -> Ordering {
	left.attempt_number
		.cmp(&right.attempt_number)
		.then_with(|| left.updated_at_unix.cmp(&right.updated_at_unix))
		.then_with(|| left.run_id.cmp(&right.run_id))
}

fn compare_project_run_status(left: &ProjectRunStatus, right: &ProjectRunStatus) -> Ordering {
	right
		.active_lease
		.cmp(&left.active_lease)
		.then_with(|| right.updated_at.cmp(&left.updated_at))
		.then_with(|| right.attempt_number.cmp(&left.attempt_number))
		.then_with(|| right.run_id.cmp(&left.run_id))
}

#[cfg(unix)]
fn clear_close_on_exec(file: &File) -> Result<()> {
	let fd = file.as_raw_fd();
	let existing_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };

	if existing_flags == -1 {
		return Err(Error::last_os_error().into());
	}

	let new_flags = existing_flags & !libc::FD_CLOEXEC;

	if new_flags != existing_flags {
		let result = unsafe { libc::fcntl(fd, libc::F_SETFD, new_flags) };

		if result == -1 {
			return Err(Error::last_os_error().into());
		}
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	#[cfg(unix)] use std::os::fd::IntoRawFd;
	use std::{collections::HashMap, path::Path};

	use tempfile::TempDir;

	use crate::state::{PreacquiredLeaseGuards, StateStore};

	const IN_PROGRESS_STATE: &str = "In Progress";

	#[test]
	fn manages_issue_leases() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store
			.upsert_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
			.expect("lease should be inserted");

		let lease = store
			.lease_for_issue("PUB-101")
			.expect("lease read should succeed")
			.expect("lease should exist");

		assert_eq!(lease.issue_id(), "PUB-101");
		assert_eq!(lease.run_id(), "run-1");
		assert_eq!(lease.project_id(), "pubfi");
		assert_eq!(lease.issue_state(), IN_PROGRESS_STATE);

		store.clear_lease("PUB-101").expect("lease should be deleted");

		assert!(store.lease_for_issue("PUB-101").expect("lease lookup should succeed").is_none());
	}

	#[test]
	fn tracks_issue_specific_leases_without_project_limit() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		assert!(
			store
				.try_acquire_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
				.expect("first lease acquisition should succeed")
		);
		assert!(
			store
				.try_acquire_lease("pubfi", "PUB-102", "run-2", IN_PROGRESS_STATE)
				.expect("second lease acquisition should succeed for another issue")
		);
		assert!(
			!store
				.try_acquire_lease("pubfi", "PUB-101", "run-3", IN_PROGRESS_STATE)
				.expect("duplicate issue acquisition should be rejected")
		);
		assert!(
			store
				.try_acquire_lease("other", "PUB-201", "run-4", IN_PROGRESS_STATE)
				.expect("other project should still acquire its own slot")
		);
	}

	#[test]
	fn shared_dispatch_slots_honor_configured_limit_across_process_local_stores() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let store_one = StateStore::open_in_memory().expect("first store should open");
		let store_two = StateStore::open_in_memory().expect("second store should open");
		let store_three = StateStore::open_in_memory().expect("third store should open");

		store_one
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("first store should configure dispatch slot root");
		store_two
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("second store should configure dispatch slot root");
		store_three
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("third store should configure dispatch slot root");

		assert!(
			store_one
				.try_acquire_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
				.expect("first shared lease acquisition should succeed")
		);
		assert!(
			store_two
				.try_acquire_lease("pubfi", "PUB-102", "run-2", IN_PROGRESS_STATE)
				.expect("second store should acquire the second shared slot")
		);
		assert!(
			!store_three
				.try_acquire_lease("pubfi", "PUB-103", "run-3", IN_PROGRESS_STATE)
				.expect("third store should observe the configured shared slots as busy")
		);

		store_one.clear_lease("PUB-101").expect("shared lease should clear");

		assert!(
			store_three
				.try_acquire_lease("pubfi", "PUB-103", "run-3", IN_PROGRESS_STATE)
				.expect("shared slot should reopen after one of the configured leases clears")
		);
	}

	#[test]
	fn failed_shared_slot_attempt_releases_issue_claim_before_retry() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let store_one = StateStore::open_in_memory().expect("first store should open");
		let store_two = StateStore::open_in_memory().expect("second store should open");

		store_one
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 1)
			.expect("first store should configure dispatch slot root");
		store_two
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 1)
			.expect("second store should configure dispatch slot root");

		assert!(
			store_one
				.try_acquire_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
				.expect("first store should acquire the only shared slot")
		);
		assert!(
			!store_two
				.try_acquire_lease("pubfi", "PUB-102", "run-2", IN_PROGRESS_STATE)
				.expect("second store should fail while the only slot is busy")
		);

		store_one.clear_lease("PUB-101").expect("shared lease should clear");

		assert!(
			store_two
				.try_acquire_lease("pubfi", "PUB-102", "run-2", IN_PROGRESS_STATE)
				.expect("retry should succeed after the failed contender releases its issue claim")
		);
	}

	#[test]
	fn shared_issue_claim_blocks_duplicate_issue_across_process_local_stores() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let store_one = StateStore::open_in_memory().expect("first store should open");
		let store_two = StateStore::open_in_memory().expect("second store should open");

		store_one
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("first store should configure dispatch slot root");
		store_two
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("second store should configure dispatch slot root");

		assert!(
			store_one
				.try_acquire_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
				.expect("first issue claim should succeed")
		);
		assert!(
			!store_two
				.try_acquire_lease("pubfi", "PUB-101", "run-2", IN_PROGRESS_STATE)
				.expect("duplicate issue claim should be rejected across processes")
		);
		assert!(
			store_two
				.try_acquire_lease("pubfi", "PUB-102", "run-3", IN_PROGRESS_STATE)
				.expect("another issue should still be able to use the remaining slot")
		);
	}

	#[test]
	fn shared_issue_claim_reopens_same_issue_after_clear_across_process_local_stores() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let store_one = StateStore::open_in_memory().expect("first store should open");
		let store_two = StateStore::open_in_memory().expect("second store should open");

		store_one
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("first store should configure dispatch slot root");
		store_two
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("second store should configure dispatch slot root");

		assert!(
			store_one
				.try_acquire_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
				.expect("first issue claim should succeed")
		);
		assert!(
			!store_two
				.try_acquire_lease("pubfi", "PUB-101", "run-2", IN_PROGRESS_STATE)
				.expect("duplicate issue claim should be rejected while the first lease is active")
		);

		store_one.clear_lease("PUB-101").expect("shared issue claim should clear");

		assert!(
			store_two
				.try_acquire_lease("pubfi", "PUB-101", "run-2", IN_PROGRESS_STATE)
				.expect("same issue claim should reopen after the first lease clears")
		);
	}

	#[test]
	fn shared_issue_claim_listing_reports_other_process_state() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let remote_store = StateStore::open_in_memory().expect("remote store should open");
		let observer_store = StateStore::open_in_memory().expect("observer store should open");

		remote_store
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("remote store should configure dispatch slot root");
		observer_store
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("observer store should configure dispatch slot root");

		assert!(
			remote_store
				.try_acquire_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
				.expect("remote issue claim should succeed")
		);

		let leases = observer_store
			.list_active_shared_leases("pubfi")
			.expect("shared claim listing should succeed");

		assert_eq!(leases.len(), 1);
		assert_eq!(leases[0].issue_id(), "PUB-101");
		assert_eq!(leases[0].run_id(), "run-1");
		assert_eq!(leases[0].issue_state(), IN_PROGRESS_STATE);
	}

	#[test]
	fn shared_state_limit_blocks_duplicate_state_across_process_local_stores() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let store_one = StateStore::open_in_memory().expect("first store should open");
		let store_two = StateStore::open_in_memory().expect("second store should open");
		let mut state_limits = HashMap::new();

		state_limits.insert(String::from(IN_PROGRESS_STATE), 1);
		store_one
			.configure_dispatch_slot_policy("pubfi", temp_dir.path(), 2, &state_limits)
			.expect("first store should configure dispatch policy");
		store_two
			.configure_dispatch_slot_policy("pubfi", temp_dir.path(), 2, &state_limits)
			.expect("second store should configure dispatch policy");

		assert!(
			store_one
				.try_acquire_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
				.expect("first in-progress lease should succeed")
		);
		assert!(
			!store_two
				.try_acquire_lease("pubfi", "PUB-102", "run-2", IN_PROGRESS_STATE)
				.expect("second in-progress lease should respect the shared state cap")
		);
		assert!(
			store_two
				.try_acquire_lease("pubfi", "PUB-103", "run-3", "Todo")
				.expect("an unbounded state should still use the remaining global slot")
		);
	}

	#[cfg(unix)]
	#[test]
	fn adopted_dispatch_slot_blocks_after_parent_releases_local_guard() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let parent_store = StateStore::open_in_memory().expect("parent store should open");
		let child_store = StateStore::open_in_memory().expect("child store should open");
		let contender_store = StateStore::open_in_memory().expect("contender store should open");

		parent_store
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 1)
			.expect("parent store should configure dispatch slot root");
		child_store
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 1)
			.expect("child store should configure dispatch slot root");
		contender_store
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 1)
			.expect("contender store should configure dispatch slot root");

		assert!(
			parent_store
				.try_acquire_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
				.expect("parent should acquire the shared slot")
		);

		let child_issue_claim = parent_store
			.clone_issue_claim_for_child("PUB-101")
			.expect("child should inherit the shared issue-claim fd");
		let (child_guard, child_slot_index) = parent_store
			.clone_dispatch_slot_for_child("PUB-101")
			.expect("child should inherit the shared dispatch-slot fd");

		child_store
			.adopt_preacquired_lease(
				"pubfi",
				"PUB-101",
				"run-1",
				IN_PROGRESS_STATE,
				PreacquiredLeaseGuards {
					issue_claim_fd: child_issue_claim.into_raw_fd(),
					dispatch_slot_fd: child_guard.into_raw_fd(),
					dispatch_slot_index: child_slot_index,
				},
			)
			.expect("child should adopt the inherited lease guard");
		parent_store
			.release_dispatch_slot("PUB-101")
			.expect("parent should release its local guard after handoff");

		assert!(
			!contender_store
				.try_acquire_lease("pubfi", "PUB-102", "run-2", IN_PROGRESS_STATE)
				.expect("child-held guard should keep the slot busy")
		);

		child_store.clear_lease("PUB-101").expect("child lease should clear");
	}

	#[cfg(unix)]
	#[test]
	fn adopted_issue_claim_blocks_same_issue_after_parent_clears_local_guard() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let parent_store = StateStore::open_in_memory().expect("parent store should open");
		let child_store = StateStore::open_in_memory().expect("child store should open");
		let contender_store = StateStore::open_in_memory().expect("contender store should open");

		parent_store
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("parent store should configure dispatch slot root");
		child_store
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("child store should configure dispatch slot root");
		contender_store
			.configure_dispatch_slot_root("pubfi", temp_dir.path(), 2)
			.expect("contender store should configure dispatch slot root");

		assert!(
			parent_store
				.try_acquire_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
				.expect("parent should acquire the shared issue claim")
		);

		let child_issue_claim = parent_store
			.clone_issue_claim_for_child("PUB-101")
			.expect("child should inherit the shared issue-claim fd");
		let (child_guard, child_slot_index) = parent_store
			.clone_dispatch_slot_for_child("PUB-101")
			.expect("child should inherit the shared dispatch-slot fd");

		child_store
			.adopt_preacquired_lease(
				"pubfi",
				"PUB-101",
				"run-1",
				IN_PROGRESS_STATE,
				PreacquiredLeaseGuards {
					issue_claim_fd: child_issue_claim.into_raw_fd(),
					dispatch_slot_fd: child_guard.into_raw_fd(),
					dispatch_slot_index: child_slot_index,
				},
			)
			.expect("child should adopt the inherited lease guard");
		parent_store
			.clear_lease("PUB-101")
			.expect("parent should drop its local lease without unlocking the child handoff");

		assert!(
			!contender_store
				.try_acquire_lease("pubfi", "PUB-101", "run-2", IN_PROGRESS_STATE)
				.expect(
					"same issue should stay claimed while the child still holds the handoff fd"
				)
		);

		child_store.clear_lease("PUB-101").expect("child lease should clear");
	}

	#[test]
	fn records_run_attempts_and_events() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store
			.record_run_attempt("run-1", "PUB-101", 1, "running")
			.expect("run attempt should be recorded");
		store.update_run_thread("run-1", "thread-1").expect("thread id should be attached");
		store
			.append_event("run-1", 1, "turn/started", "{\"turn\":\"1\"}")
			.expect("event should be recorded");

		let run_attempt = store
			.run_attempt("run-1")
			.expect("run attempt query should succeed")
			.expect("run attempt should exist");

		assert_eq!(run_attempt.issue_id(), "PUB-101");
		assert_eq!(run_attempt.attempt_number(), 1);
		assert_eq!(run_attempt.status(), "running");
		assert_eq!(run_attempt.thread_id(), Some("thread-1"));
		assert_eq!(store.event_count("run-1").expect("event count should succeed"), 1);
		assert_eq!(store.next_attempt_number("PUB-101").expect("next attempt should load"), 2);
		assert_eq!(
			store.retry_budget_attempt_count("PUB-101").expect("retry budget count should load"),
			0
		);

		store.update_run_status("run-1", "interrupted").expect("status should update");

		let updated = store
			.run_attempt("run-1")
			.expect("run attempt query should succeed")
			.expect("run attempt should exist");

		assert_eq!(updated.status(), "interrupted");
		assert!(
			store
				.last_run_activity_unix_epoch("run-1")
				.expect("last activity lookup should succeed")
				.is_some()
		);
	}

	#[test]
	fn counts_retry_budget_attempts_per_issue() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store
			.record_run_attempt("run-1", "PUB-101", 1, "succeeded")
			.expect("first run should record");
		store
			.record_run_attempt("run-2", "PUB-101", 2, "failed")
			.expect("second run should record");
		store
			.record_run_attempt("run-3", "PUB-101", 3, "interrupted")
			.expect("third run should record");
		store
			.record_run_attempt("run-5", "PUB-101", 4, "terminal_guarded")
			.expect("guarded run should record");
		store
			.record_run_attempt("run-4", "PUB-102", 1, "failed")
			.expect("other issue run should record");

		assert_eq!(
			store.retry_budget_attempt_count("PUB-101").expect("retry budget count should load"),
			3
		);
		assert_eq!(
			store.retry_budget_attempt_count("PUB-102").expect("retry budget count should load"),
			1
		);
	}

	#[test]
	fn loads_latest_run_attempt_for_issue() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store.record_run_attempt("run-1", "PUB-101", 1, "failed").expect("first run should record");
		store
			.record_run_attempt("run-2", "PUB-101", 2, "terminal_guarded")
			.expect("latest run should record");

		let attempt = store
			.latest_run_attempt_for_issue("PUB-101")
			.expect("latest run lookup should succeed")
			.expect("latest run should exist");

		assert_eq!(attempt.run_id(), "run-2");
		assert_eq!(attempt.attempt_number(), 2);
		assert_eq!(attempt.status(), "terminal_guarded");
	}

	#[test]
	fn manages_workspace_mappings() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store
			.upsert_workspace("pubfi", "PUB-101", "x/pub-101", "/tmp/workspaces/pub-101")
			.expect("workspace mapping should be recorded");

		let mapping = store
			.workspace_for_issue("PUB-101")
			.expect("mapping lookup should succeed")
			.expect("mapping should exist");

		assert_eq!(mapping.issue_id(), "PUB-101");
		assert_eq!(mapping.branch_name(), "x/pub-101");
		assert_eq!(mapping.workspace_path(), Path::new("/tmp/workspaces/pub-101"));
		assert_eq!(mapping.project_id(), "pubfi");
		assert_eq!(store.list_workspaces("pubfi").expect("list should succeed").len(), 1);

		store.clear_workspace("PUB-101").expect("mapping should be deleted");

		assert!(store.workspace_for_issue("PUB-101").expect("lookup should succeed").is_none());
	}

	#[test]
	fn lists_issue_leases() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store
			.upsert_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
			.expect("first lease should be inserted");
		store
			.upsert_lease("pubfi", "PUB-102", "run-2", IN_PROGRESS_STATE)
			.expect("second lease should be inserted");

		let leases = store.list_leases("pubfi").expect("lease listing should succeed");

		assert_eq!(leases.len(), 2);
		assert_eq!(leases[0].project_id(), "pubfi");
		assert_eq!(leases[0].issue_id(), "PUB-101");
		assert_eq!(leases[1].issue_id(), "PUB-102");
	}

	#[test]
	fn lists_recent_project_runs_with_protocol_summary() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store
			.record_run_attempt("run-2", "PUB-102", 2, "failed")
			.expect("older run attempt should be recorded");
		store
			.record_run_attempt("run-1", "PUB-101", 1, "running")
			.expect("active run attempt should be recorded");
		store.update_run_thread("run-1", "thread-1").expect("thread id should attach");
		store
			.upsert_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
			.expect("lease should record");
		store
			.upsert_workspace("pubfi", "PUB-101", "x/pubfi-pub-101", "/tmp/workspaces/pub-101")
			.expect("active workspace should record");
		store
			.upsert_workspace("pubfi", "PUB-102", "x/pubfi-pub-102", "/tmp/workspaces/pub-102")
			.expect("retained workspace should record");
		store
			.append_event("run-1", 1, "turn/started", "{\"turn\":\"1\"}")
			.expect("event should record");
		store
			.append_event("run-1", 2, "turn/completed", "{\"turn\":\"1\"}")
			.expect("second event should record");

		let runs = store.list_recent_runs("pubfi", 10).expect("recent project runs should load");

		assert_eq!(runs.len(), 2);
		assert_eq!(runs[0].run_id(), "run-1");
		assert!(runs[0].active_lease());
		assert_eq!(runs[0].thread_id(), Some("thread-1"));
		assert_eq!(runs[0].event_count(), 2);
		assert_eq!(runs[0].last_event_type(), Some("turn/completed"));
		assert_eq!(runs[0].branch_name(), Some("x/pubfi-pub-101"));
		assert_eq!(runs[0].workspace_path(), Some(Path::new("/tmp/workspaces/pub-101")));
		assert_eq!(runs[1].run_id(), "run-2");
		assert!(!runs[1].active_lease());
		assert_eq!(runs[1].event_count(), 0);
	}

	#[test]
	fn lists_active_project_runs_only() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store
			.record_run_attempt("run-1", "PUB-101", 1, "running")
			.expect("first run should record");
		store
			.record_run_attempt("run-2", "PUB-102", 1, "running")
			.expect("second run should record");
		store
			.upsert_lease("pubfi", "PUB-101", "run-1", IN_PROGRESS_STATE)
			.expect("lease should record");
		store
			.upsert_lease("other", "PUB-102", "run-2", IN_PROGRESS_STATE)
			.expect("other-project lease should record");
		store
			.upsert_workspace("pubfi", "PUB-101", "x/pubfi-pub-101", "/tmp/workspaces/pub-101")
			.expect("first workspace should record");
		store
			.upsert_workspace("other", "PUB-102", "x/other-pub-102", "/tmp/workspaces/pub-102")
			.expect("second workspace should record");

		let runs = store.list_active_runs("pubfi").expect("active project runs should load");

		assert_eq!(runs.len(), 1);
		assert_eq!(runs[0].run_id(), "run-1");
		assert!(runs[0].active_lease());
	}
}
