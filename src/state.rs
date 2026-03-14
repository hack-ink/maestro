//! Thin local persistence for active Maestro execution state.

use std::{
	fs,
	path::{Path, PathBuf},
	time::Duration,
};

use rusqlite::{self, Connection, OptionalExtension, Row};

use crate::prelude::eyre;

const INIT_SQL: &str = include_str!("../migrations/0001_init.sql");

/// Local state store for leases, attempts, workspaces, and protocol events.
pub struct StateStore {
	connection: Connection,
}
impl StateStore {
	/// Open or create a SQLite-backed state store on disk.
	pub fn open(path: impl AsRef<Path>) -> crate::prelude::Result<Self> {
		let path = path.as_ref();

		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}

		let connection = Connection::open(path)?;

		connection.busy_timeout(Duration::from_secs(5))?;

		let store = Self { connection };

		store.initialize()?;

		Ok(store)
	}

	/// Open an in-memory state store for tests and probes.
	pub fn open_in_memory() -> crate::prelude::Result<Self> {
		let connection = Connection::open_in_memory()?;

		connection.busy_timeout(Duration::from_secs(5))?;

		let store = Self { connection };

		store.initialize()?;

		Ok(store)
	}

	/// Create or replace the active lease for one issue.
	pub fn upsert_lease(
		&self,
		project_id: &str,
		issue_id: &str,
		run_id: &str,
	) -> crate::prelude::Result<()> {
		self.connection.execute(
			"
			INSERT INTO issue_leases (project_id, issue_id, run_id)
			VALUES (?1, ?2, ?3)
			ON CONFLICT(issue_id) DO UPDATE SET
				project_id = excluded.project_id,
				run_id = excluded.run_id,
				acquired_at = CURRENT_TIMESTAMP
			",
			rusqlite::params![project_id, issue_id, run_id],
		)?;

		Ok(())
	}

	/// Read the active lease for one issue.
	pub fn lease_for_issue(&self, issue_id: &str) -> crate::prelude::Result<Option<IssueLease>> {
		let lease = self
			.connection
			.query_row(
				"SELECT project_id, issue_id, run_id FROM issue_leases WHERE issue_id = ?1",
				rusqlite::params![issue_id],
				|row| {
					Ok(IssueLease {
						project_id: row.get(0)?,
						issue_id: row.get(1)?,
						run_id: row.get(2)?,
					})
				},
			)
			.optional()?;

		Ok(lease)
	}

	/// List all active leases.
	pub fn list_leases(&self, project_id: &str) -> crate::prelude::Result<Vec<IssueLease>> {
		let mut statement = self.connection.prepare(
			"
			SELECT project_id, issue_id, run_id
			FROM issue_leases
			WHERE project_id = ?1
			ORDER BY issue_id
			",
		)?;
		let rows = statement.query_map(rusqlite::params![project_id], |row| {
			Ok(IssueLease { project_id: row.get(0)?, issue_id: row.get(1)?, run_id: row.get(2)? })
		})?;
		let leases = rows.collect::<rusqlite::Result<Vec<_>>>()?;

		Ok(leases)
	}

	/// Remove the active lease for one issue.
	pub fn clear_lease(&self, issue_id: &str) -> crate::prelude::Result<()> {
		self.connection
			.execute("DELETE FROM issue_leases WHERE issue_id = ?1", rusqlite::params![issue_id])?;

		Ok(())
	}

	/// Insert or update a run attempt record.
	pub fn record_run_attempt(
		&self,
		run_id: &str,
		issue_id: &str,
		attempt_number: i64,
		status: &str,
	) -> crate::prelude::Result<()> {
		self.connection.execute(
			"
			INSERT INTO run_attempts (run_id, issue_id, attempt_number, status)
			VALUES (?1, ?2, ?3, ?4)
			ON CONFLICT(run_id) DO UPDATE SET
				issue_id = excluded.issue_id,
				attempt_number = excluded.attempt_number,
				status = excluded.status,
				updated_at = CURRENT_TIMESTAMP
			",
			rusqlite::params![run_id, issue_id, attempt_number, status],
		)?;

		Ok(())
	}

	/// Compute the next attempt number for one issue.
	pub fn next_attempt_number(&self, issue_id: &str) -> crate::prelude::Result<i64> {
		let next_attempt = self.connection.query_row(
			"
			SELECT COALESCE(MAX(attempt_number), 0) + 1
			FROM run_attempts
			WHERE issue_id = ?1
			",
			rusqlite::params![issue_id],
			|row| row.get(0),
		)?;

		Ok(next_attempt)
	}

	/// Attach the active thread identifier to a run attempt.
	pub fn update_run_thread(&self, run_id: &str, thread_id: &str) -> crate::prelude::Result<()> {
		self.connection.execute(
			"
			UPDATE run_attempts
			SET thread_id = ?2,
				updated_at = CURRENT_TIMESTAMP
			WHERE run_id = ?1
			",
			rusqlite::params![run_id, thread_id],
		)?;

		Ok(())
	}

	/// Update the status for one run attempt.
	pub fn update_run_status(&self, run_id: &str, status: &str) -> crate::prelude::Result<()> {
		self.connection.execute(
			"
			UPDATE run_attempts
			SET status = ?2,
				updated_at = CURRENT_TIMESTAMP
			WHERE run_id = ?1
			",
			rusqlite::params![run_id, status],
		)?;

		Ok(())
	}

	/// Read one run attempt.
	pub fn run_attempt(&self, run_id: &str) -> crate::prelude::Result<Option<RunAttempt>> {
		let attempt = self
			.connection
			.query_row(
				"
				SELECT run_id, issue_id, attempt_number, status, thread_id
				FROM run_attempts
				WHERE run_id = ?1
				",
				rusqlite::params![run_id],
				|row| {
					Ok(RunAttempt {
						run_id: row.get(0)?,
						issue_id: row.get(1)?,
						attempt_number: row.get(2)?,
						status: row.get(3)?,
						thread_id: row.get(4)?,
					})
				},
			)
			.optional()?;

		Ok(attempt)
	}

	/// List recent run attempts for one project, including lease and protocol summary fields.
	pub fn list_recent_runs(
		&self,
		project_id: &str,
		limit: usize,
	) -> crate::prelude::Result<Vec<ProjectRunStatus>> {
		let mut statement = self.connection.prepare(&format!(
			"
			{}
			WHERE m.project_id = ?1
				OR EXISTS(
					SELECT 1
					FROM issue_leases l
					WHERE l.project_id = ?1
						AND l.issue_id = r.issue_id
						AND l.run_id = r.run_id
				)
			ORDER BY active_lease DESC, r.updated_at DESC, r.attempt_number DESC, r.run_id DESC
			LIMIT ?2
			",
			project_run_status_select_sql()
		))?;
		let rows = statement
			.query_map(rusqlite::params![project_id, limit as i64], map_project_run_status_row)?;
		let runs = rows.collect::<rusqlite::Result<Vec<_>>>()?;

		Ok(runs)
	}

	/// List all active leased runs for one project without applying the recent-run limit.
	pub fn list_active_runs(
		&self,
		project_id: &str,
	) -> crate::prelude::Result<Vec<ProjectRunStatus>> {
		let mut statement = self.connection.prepare(&format!(
			"
			{}
			WHERE EXISTS(
				SELECT 1
				FROM issue_leases l
				WHERE l.project_id = ?1
					AND l.issue_id = r.issue_id
					AND l.run_id = r.run_id
			)
			ORDER BY r.updated_at DESC, r.attempt_number DESC, r.run_id DESC
			",
			project_run_status_select_sql()
		))?;
		let rows =
			statement.query_map(rusqlite::params![project_id], map_project_run_status_row)?;
		let runs = rows.collect::<rusqlite::Result<Vec<_>>>()?;

		Ok(runs)
	}

	/// Append one protocol event to the journal for a run.
	pub fn append_event(
		&self,
		run_id: &str,
		sequence_number: i64,
		event_type: &str,
		payload: &str,
	) -> crate::prelude::Result<()> {
		self.connection.execute(
			"
			INSERT INTO event_journal (run_id, sequence_number, event_type, payload)
			VALUES (?1, ?2, ?3, ?4)
			",
			rusqlite::params![run_id, sequence_number, event_type, payload],
		)?;

		Ok(())
	}

	/// Count protocol journal records for one run.
	pub fn event_count(&self, run_id: &str) -> crate::prelude::Result<i64> {
		let count = self.connection.query_row(
			"SELECT COUNT(*) FROM event_journal WHERE run_id = ?1",
			rusqlite::params![run_id],
			|row| row.get(0),
		)?;

		Ok(count)
	}

	/// Read the latest persisted activity timestamp for one run as a Unix epoch.
	pub fn last_run_activity_unix_epoch(
		&self,
		run_id: &str,
	) -> crate::prelude::Result<Option<i64>> {
		let latest_activity = self.connection.query_row(
			"
			SELECT MAX(ts)
			FROM (
				SELECT CAST(strftime('%s', updated_at) AS INTEGER) AS ts
				FROM run_attempts
				WHERE run_id = ?1
				UNION ALL
				SELECT CAST(strftime('%s', created_at) AS INTEGER) AS ts
				FROM event_journal
				WHERE run_id = ?1
			)
			",
			rusqlite::params![run_id],
			|row| row.get(0),
		)?;

		Ok(latest_activity)
	}

	/// Read the latest persisted protocol-event timestamp for one run as a Unix epoch.
	pub fn last_protocol_activity_unix_epoch(
		&self,
		run_id: &str,
	) -> crate::prelude::Result<Option<i64>> {
		let latest_activity = self.connection.query_row(
			"
			SELECT MAX(CAST(strftime('%s', created_at) AS INTEGER))
			FROM event_journal
			WHERE run_id = ?1
			",
			rusqlite::params![run_id],
			|row| row.get(0),
		)?;

		Ok(latest_activity)
	}

	/// Create or replace the workspace mapping for one issue.
	pub fn upsert_workspace(
		&self,
		project_id: &str,
		issue_id: &str,
		branch_name: &str,
		workspace_path: &str,
	) -> crate::prelude::Result<()> {
		self.connection.execute(
			"
			INSERT INTO workspace_mappings (project_id, issue_id, branch_name, workspace_path)
			VALUES (?1, ?2, ?3, ?4)
			ON CONFLICT(issue_id) DO UPDATE SET
				project_id = excluded.project_id,
				branch_name = excluded.branch_name,
				workspace_path = excluded.workspace_path,
				updated_at = CURRENT_TIMESTAMP
			",
			rusqlite::params![project_id, issue_id, branch_name, workspace_path],
		)?;

		Ok(())
	}

	/// Read the workspace mapping for one issue.
	pub fn workspace_for_issue(
		&self,
		issue_id: &str,
	) -> crate::prelude::Result<Option<WorkspaceMapping>> {
		let mapping = self
			.connection
			.query_row(
				"
				SELECT project_id, issue_id, branch_name, workspace_path
				FROM workspace_mappings
				WHERE issue_id = ?1
				",
				rusqlite::params![issue_id],
				|row| {
					Ok(WorkspaceMapping {
						project_id: row.get(0)?,
						issue_id: row.get(1)?,
						branch_name: row.get(2)?,
						workspace_path: PathBuf::from(row.get::<_, String>(3)?),
					})
				},
			)
			.optional()?;

		Ok(mapping)
	}

	/// List all known workspace mappings.
	pub fn list_workspaces(
		&self,
		project_id: &str,
	) -> crate::prelude::Result<Vec<WorkspaceMapping>> {
		let mut statement = self.connection.prepare(
			"
			SELECT project_id, issue_id, branch_name, workspace_path
			FROM workspace_mappings
			WHERE project_id = ?1
			ORDER BY issue_id
			",
		)?;
		let rows = statement.query_map(rusqlite::params![project_id], |row| {
			Ok(WorkspaceMapping {
				project_id: row.get(0)?,
				issue_id: row.get(1)?,
				branch_name: row.get(2)?,
				workspace_path: PathBuf::from(row.get::<_, String>(3)?),
			})
		})?;
		let mappings = rows.collect::<rusqlite::Result<Vec<_>>>()?;

		Ok(mappings)
	}

	/// Remove the workspace mapping for one issue.
	pub fn clear_workspace(&self, issue_id: &str) -> crate::prelude::Result<()> {
		self.connection.execute(
			"DELETE FROM workspace_mappings WHERE issue_id = ?1",
			rusqlite::params![issue_id],
		)?;

		Ok(())
	}

	fn initialize(&self) -> crate::prelude::Result<()> {
		if legacy_workspace_table_exists(&self.connection)? {
			eyre::bail!(
				"Unsupported local state schema: found pre-workspace table `worktree_mappings`. Remove or reset the local state database before running this build."
			);
		}

		self.connection.execute_batch(INIT_SQL)?;

		Ok(())
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

	/// Number of persisted protocol events for the run.
	pub fn event_count(&self) -> i64 {
		self.event_count
	}

	/// Latest persisted protocol event type, when one exists.
	pub fn last_event_type(&self) -> Option<&str> {
		self.last_event_type.as_deref()
	}

	/// Timestamp of the latest persisted protocol event, when one exists.
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

fn legacy_workspace_table_exists(connection: &Connection) -> crate::prelude::Result<bool> {
	let exists = connection.query_row(
		"
		SELECT EXISTS(
			SELECT 1
			FROM sqlite_master
			WHERE type = 'table'
				AND name = 'worktree_mappings'
		)
		",
		[],
		|row| row.get::<_, i64>(0),
	)?;

	Ok(exists != 0)
}

fn project_run_status_select_sql() -> &'static str {
	"
	SELECT
		r.run_id,
		r.issue_id,
		r.attempt_number,
		r.status,
		r.thread_id,
		r.updated_at,
		m.branch_name,
		m.workspace_path,
		EXISTS(
			SELECT 1
			FROM issue_leases l
			WHERE l.project_id = ?1
				AND l.issue_id = r.issue_id
				AND l.run_id = r.run_id
		) AS active_lease,
		(
			SELECT COUNT(*)
			FROM event_journal e
			WHERE e.run_id = r.run_id
		) AS event_count,
		(
			SELECT e.event_type
			FROM event_journal e
			WHERE e.run_id = r.run_id
			ORDER BY e.sequence_number DESC
			LIMIT 1
		) AS last_event_type,
		(
			SELECT e.created_at
			FROM event_journal e
			WHERE e.run_id = r.run_id
			ORDER BY e.sequence_number DESC
			LIMIT 1
		) AS last_event_at
	FROM run_attempts r
	LEFT JOIN workspace_mappings m
		ON m.issue_id = r.issue_id
		AND m.project_id = ?1
	"
}

fn map_project_run_status_row(row: &Row<'_>) -> rusqlite::Result<ProjectRunStatus> {
	Ok(ProjectRunStatus {
		run_id: row.get(0)?,
		issue_id: row.get(1)?,
		attempt_number: row.get(2)?,
		status: row.get(3)?,
		thread_id: row.get(4)?,
		updated_at: row.get(5)?,
		branch_name: row.get(6)?,
		workspace_path: row.get::<_, Option<String>>(7)?.map(PathBuf::from),
		active_lease: row.get::<_, i64>(8)? != 0,
		event_count: row.get(9)?,
		last_event_type: row.get(10)?,
		last_event_at: row.get(11)?,
	})
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use tempfile::NamedTempFile;

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

		let runs = store.list_recent_runs("pubfi", 10).expect("status query should succeed");

		assert_eq!(runs.len(), 2);
		assert_eq!(runs[0].run_id(), "run-1");
		assert!(runs[0].active_lease());
		assert_eq!(runs[0].branch_name(), Some("x/pubfi-pub-101"));
		assert_eq!(runs[0].workspace_path(), Some(Path::new("/tmp/workspaces/pub-101")));
		assert_eq!(runs[0].event_count(), 2);
		assert_eq!(runs[0].last_event_type(), Some("turn/completed"));
		assert_eq!(runs[0].thread_id(), Some("thread-1"));
		assert_eq!(runs[1].run_id(), "run-2");
		assert!(!runs[1].active_lease());
		assert_eq!(runs[1].branch_name(), Some("x/pubfi-pub-102"));
		assert_eq!(runs[1].event_count(), 0);
		assert_eq!(runs[1].last_event_type(), None);
	}

	#[test]
	fn recent_project_runs_respect_limit() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store.record_run_attempt("run-1", "PUB-101", 1, "failed").expect("first run should record");
		store
			.record_run_attempt("run-2", "PUB-102", 1, "failed")
			.expect("second run should record");
		store
			.upsert_workspace("pubfi", "PUB-101", "x/pubfi-pub-101", "/tmp/workspaces/pub-101")
			.expect("first workspace should record");
		store
			.upsert_workspace("pubfi", "PUB-102", "x/pubfi-pub-102", "/tmp/workspaces/pub-102")
			.expect("second workspace should record");

		let runs = store.list_recent_runs("pubfi", 1).expect("status query should succeed");

		assert_eq!(runs.len(), 1);
	}

	#[test]
	fn active_project_runs_are_not_capped_by_recent_limit() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store
			.record_run_attempt("run-1", "PUB-101", 1, "running")
			.expect("first active run should record");
		store
			.record_run_attempt("run-2", "PUB-102", 1, "running")
			.expect("second active run should record");
		store.upsert_lease("pubfi", "PUB-101", "run-1").expect("first lease should record");
		store.upsert_lease("pubfi", "PUB-102", "run-2").expect("second lease should record");
		store
			.upsert_workspace("pubfi", "PUB-101", "x/pubfi-pub-101", "/tmp/workspaces/pub-101")
			.expect("first workspace should record");
		store
			.upsert_workspace("pubfi", "PUB-102", "x/pubfi-pub-102", "/tmp/workspaces/pub-102")
			.expect("second workspace should record");

		let recent_runs = store.list_recent_runs("pubfi", 1).expect("recent runs should query");
		let active_runs = store.list_active_runs("pubfi").expect("active runs should query");

		assert_eq!(recent_runs.len(), 1);
		assert_eq!(active_runs.len(), 2);
		assert!(active_runs.iter().all(|run| run.active_lease()));
	}

	#[test]
	fn rejects_legacy_worktree_mappings_table_on_open() {
		let db_file = NamedTempFile::new().expect("temp db file should exist");
		let connection =
			rusqlite::Connection::open(db_file.path()).expect("raw sqlite connection should open");

		connection
			.execute_batch(
				"
				CREATE TABLE worktree_mappings (
					project_id TEXT NOT NULL,
					issue_id TEXT PRIMARY KEY,
					branch_name TEXT NOT NULL,
					worktree_path TEXT NOT NULL,
					updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
				);
				",
			)
			.expect("legacy table should seed");

		drop(connection);

		let error = match StateStore::open(db_file.path()) {
			Ok(_) => panic!("legacy local state should be rejected"),
			Err(error) => error,
		};

		assert!(error.to_string().contains(
			"Unsupported local state schema: found pre-workspace table `worktree_mappings`"
		));
	}
}
