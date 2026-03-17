//! Thin in-memory runtime state for active Maestro execution.

use std::{
	cmp::Ordering,
	collections::HashMap,
	fs::{self, File, OpenOptions, TryLockError},
	io::ErrorKind,
	path::{Path, PathBuf},
	process,
	sync::{Mutex, MutexGuard},
};

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::prelude::{Result, eyre};

pub(crate) const RUN_ACTIVITY_MARKER_FILE: &str = ".maestro-run-activity";

const DISPATCH_SLOT_LOCK_FILE: &str = ".maestro-dispatch-slot.lock";

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
	) -> Result<()> {
		let mut state = self.lock()?;

		state
			.dispatch_lock_roots
			.insert(project_id.to_owned(), workspace_root.as_ref().to_path_buf());

		Ok(())
	}

	/// Create or replace the active lease for one issue.
	pub fn upsert_lease(&self, project_id: &str, issue_id: &str, run_id: &str) -> Result<()> {
		let mut state = self.lock()?;

		state.leases.insert(
			issue_id.to_owned(),
			IssueLease {
				project_id: project_id.to_owned(),
				issue_id: issue_id.to_owned(),
				run_id: run_id.to_owned(),
			},
		);

		Ok(())
	}

	/// Try to acquire the project's single active dispatch slot for one issue.
	pub fn try_acquire_lease(
		&self,
		project_id: &str,
		issue_id: &str,
		run_id: &str,
	) -> Result<bool> {
		let mut state = self.lock()?;

		if state
			.leases
			.values()
			.any(|lease| lease.project_id == project_id || lease.issue_id == issue_id)
		{
			return Ok(false);
		}

		if let Some(dispatch_lock_root) = state.dispatch_lock_roots.get(project_id) {
			fs::create_dir_all(dispatch_lock_root)?;

			let lock_file = OpenOptions::new()
				.read(true)
				.write(true)
				.create(true)
				.truncate(false)
				.open(dispatch_lock_root.join(DISPATCH_SLOT_LOCK_FILE))?;

			match lock_file.try_lock() {
				Ok(()) => {
					state
						.dispatch_slot_guards
						.insert(project_id.to_owned(), DispatchSlotGuard { _lock_file: lock_file });
				},
				Err(TryLockError::WouldBlock) => return Ok(false),
				Err(TryLockError::Error(error)) => return Err(error.into()),
			}
		}

		state.leases.insert(
			issue_id.to_owned(),
			IssueLease {
				project_id: project_id.to_owned(),
				issue_id: issue_id.to_owned(),
				run_id: run_id.to_owned(),
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

	/// Remove the active lease for one issue.
	pub fn clear_lease(&self, issue_id: &str) -> Result<()> {
		let mut state = self.lock()?;
		let Some(lease) = state.leases.remove(issue_id) else {
			return Ok(());
		};

		if !state.leases.values().any(|active| active.project_id() == lease.project_id()) {
			state.dispatch_slot_guards.remove(lease.project_id());
		}

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

struct DispatchSlotGuard {
	_lock_file: File,
}

#[derive(Default)]
struct StateData {
	leases: HashMap<String, IssueLease>,
	run_attempts: HashMap<String, RunAttemptRecord>,
	events: HashMap<String, Vec<ProtocolEventRecord>>,
	workspaces: HashMap<String, WorkspaceMappingRecord>,
	dispatch_lock_roots: HashMap<String, PathBuf>,
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

#[cfg(test)]
mod tests {
	use std::path::Path;

	use tempfile::TempDir;

	use crate::state::StateStore;

	#[test]
	fn manages_issue_leases() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store.upsert_lease("pubfi", "PUB-101", "run-1").expect("lease should be inserted");

		let lease = store
			.lease_for_issue("PUB-101")
			.expect("lease read should succeed")
			.expect("lease should exist");

		assert_eq!(lease.issue_id(), "PUB-101");
		assert_eq!(lease.run_id(), "run-1");
		assert_eq!(lease.project_id(), "pubfi");

		store.clear_lease("PUB-101").expect("lease should be deleted");

		assert!(store.lease_for_issue("PUB-101").expect("lease lookup should succeed").is_none());
	}

	#[test]
	fn tries_to_acquire_single_project_dispatch_slot() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		assert!(
			store
				.try_acquire_lease("pubfi", "PUB-101", "run-1")
				.expect("first lease acquisition should succeed")
		);
		assert!(
			!store
				.try_acquire_lease("pubfi", "PUB-102", "run-2")
				.expect("second lease acquisition should be rejected")
		);
		assert!(
			!store
				.try_acquire_lease("pubfi", "PUB-101", "run-3")
				.expect("duplicate issue acquisition should be rejected")
		);
		assert!(
			store
				.try_acquire_lease("other", "PUB-201", "run-4")
				.expect("other project should still acquire its own slot")
		);
	}

	#[test]
	fn shared_dispatch_slot_blocks_across_process_local_stores() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let store_one = StateStore::open_in_memory().expect("first store should open");
		let store_two = StateStore::open_in_memory().expect("second store should open");

		store_one
			.configure_dispatch_slot_root("pubfi", temp_dir.path())
			.expect("first store should configure dispatch slot root");
		store_two
			.configure_dispatch_slot_root("pubfi", temp_dir.path())
			.expect("second store should configure dispatch slot root");

		assert!(
			store_one
				.try_acquire_lease("pubfi", "PUB-101", "run-1")
				.expect("first shared lease acquisition should succeed")
		);
		assert!(
			!store_two
				.try_acquire_lease("pubfi", "PUB-102", "run-2")
				.expect("second store should observe the shared slot as busy")
		);

		store_one.clear_lease("PUB-101").expect("shared lease should clear");

		assert!(
			store_two
				.try_acquire_lease("pubfi", "PUB-102", "run-2")
				.expect("shared slot should reopen after the first lease clears")
		);
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

		store.upsert_lease("pubfi", "PUB-101", "run-1").expect("first lease should be inserted");
		store.upsert_lease("pubfi", "PUB-102", "run-2").expect("second lease should be inserted");

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
		store.upsert_lease("pubfi", "PUB-101", "run-1").expect("lease should record");
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
		store.upsert_lease("pubfi", "PUB-101", "run-1").expect("lease should record");
		store.upsert_lease("other", "PUB-102", "run-2").expect("other-project lease should record");
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
