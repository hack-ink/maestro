//! Thin local persistence for active Maestro execution state.

use std::{
	fs,
	path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};

use crate::prelude::Result;

const INIT_SQL: &str = include_str!("../migrations/0001_init.sql");

/// Local state store for leases, attempts, worktrees, and protocol events.
pub struct StateStore {
	connection: Connection,
}
impl StateStore {
	/// Open or create a SQLite-backed state store on disk.
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let path = path.as_ref();

		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}

		let connection = Connection::open(path)?;
		let store = Self { connection };

		store.initialize()?;

		Ok(store)
	}

	/// Open an in-memory state store for tests and probes.
	pub fn open_in_memory() -> Result<Self> {
		let connection = Connection::open_in_memory()?;
		let store = Self { connection };

		store.initialize()?;

		Ok(store)
	}

	/// Create or replace the active lease for one issue.
	pub fn upsert_lease(&self, project_id: &str, issue_id: &str, run_id: &str) -> Result<()> {
		self.connection.execute(
			"
			INSERT INTO issue_leases (project_id, issue_id, run_id)
			VALUES (?1, ?2, ?3)
			ON CONFLICT(issue_id) DO UPDATE SET
				project_id = excluded.project_id,
				run_id = excluded.run_id,
				acquired_at = CURRENT_TIMESTAMP
			",
			params![project_id, issue_id, run_id],
		)?;

		Ok(())
	}

	/// Read the active lease for one issue.
	pub fn lease_for_issue(&self, issue_id: &str) -> Result<Option<IssueLease>> {
		let lease = self
			.connection
			.query_row(
				"SELECT project_id, issue_id, run_id FROM issue_leases WHERE issue_id = ?1",
				params![issue_id],
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
	pub fn list_leases(&self, project_id: &str) -> Result<Vec<IssueLease>> {
		let mut statement = self.connection.prepare(
			"
			SELECT project_id, issue_id, run_id
			FROM issue_leases
			WHERE project_id = ?1
			ORDER BY issue_id
			",
		)?;
		let rows = statement.query_map(params![project_id], |row| {
			Ok(IssueLease { project_id: row.get(0)?, issue_id: row.get(1)?, run_id: row.get(2)? })
		})?;

		let leases = rows.collect::<rusqlite::Result<Vec<_>>>()?;

		Ok(leases)
	}

	/// Remove the active lease for one issue.
	pub fn clear_lease(&self, issue_id: &str) -> Result<()> {
		self.connection
			.execute("DELETE FROM issue_leases WHERE issue_id = ?1", params![issue_id])?;

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
			params![run_id, issue_id, attempt_number, status],
		)?;

		Ok(())
	}

	/// Compute the next attempt number for one issue.
	pub fn next_attempt_number(&self, issue_id: &str) -> Result<i64> {
		let next_attempt = self.connection.query_row(
			"
			SELECT COALESCE(MAX(attempt_number), 0) + 1
			FROM run_attempts
			WHERE issue_id = ?1
			",
			params![issue_id],
			|row| row.get(0),
		)?;

		Ok(next_attempt)
	}

	/// Attach the active thread identifier to a run attempt.
	pub fn update_run_thread(&self, run_id: &str, thread_id: &str) -> Result<()> {
		self.connection.execute(
			"
			UPDATE run_attempts
			SET thread_id = ?2,
				updated_at = CURRENT_TIMESTAMP
			WHERE run_id = ?1
			",
			params![run_id, thread_id],
		)?;

		Ok(())
	}

	/// Update the status for one run attempt.
	pub fn update_run_status(&self, run_id: &str, status: &str) -> Result<()> {
		self.connection.execute(
			"
			UPDATE run_attempts
			SET status = ?2,
				updated_at = CURRENT_TIMESTAMP
			WHERE run_id = ?1
			",
			params![run_id, status],
		)?;

		Ok(())
	}

	/// Read one run attempt.
	pub fn run_attempt(&self, run_id: &str) -> Result<Option<RunAttempt>> {
		let attempt = self
			.connection
			.query_row(
				"
				SELECT run_id, issue_id, attempt_number, status, thread_id
				FROM run_attempts
				WHERE run_id = ?1
				",
				params![run_id],
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

	/// Append one protocol event to the journal for a run.
	pub fn append_event(
		&self,
		run_id: &str,
		sequence_number: i64,
		event_type: &str,
		payload: &str,
	) -> Result<()> {
		self.connection.execute(
			"
			INSERT INTO event_journal (run_id, sequence_number, event_type, payload)
			VALUES (?1, ?2, ?3, ?4)
			",
			params![run_id, sequence_number, event_type, payload],
		)?;

		Ok(())
	}

	/// Count protocol journal records for one run.
	pub fn event_count(&self, run_id: &str) -> Result<i64> {
		let count = self.connection.query_row(
			"SELECT COUNT(*) FROM event_journal WHERE run_id = ?1",
			params![run_id],
			|row| row.get(0),
		)?;

		Ok(count)
	}

	/// Create or replace the worktree mapping for one issue.
	pub fn upsert_worktree(
		&self,
		project_id: &str,
		issue_id: &str,
		branch_name: &str,
		worktree_path: &str,
	) -> Result<()> {
		self.connection.execute(
			"
			INSERT INTO worktree_mappings (project_id, issue_id, branch_name, worktree_path)
			VALUES (?1, ?2, ?3, ?4)
			ON CONFLICT(issue_id) DO UPDATE SET
				project_id = excluded.project_id,
				branch_name = excluded.branch_name,
				worktree_path = excluded.worktree_path,
				updated_at = CURRENT_TIMESTAMP
			",
			params![project_id, issue_id, branch_name, worktree_path],
		)?;

		Ok(())
	}

	/// Read the worktree mapping for one issue.
	pub fn worktree_for_issue(&self, issue_id: &str) -> Result<Option<WorktreeMapping>> {
		let mapping = self
			.connection
			.query_row(
				"
				SELECT project_id, issue_id, branch_name, worktree_path
				FROM worktree_mappings
				WHERE issue_id = ?1
				",
				params![issue_id],
				|row| {
					Ok(WorktreeMapping {
						project_id: row.get(0)?,
						issue_id: row.get(1)?,
						branch_name: row.get(2)?,
						worktree_path: PathBuf::from(row.get::<_, String>(3)?),
					})
				},
			)
			.optional()?;

		Ok(mapping)
	}

	/// List all known worktree mappings.
	pub fn list_worktrees(&self, project_id: &str) -> Result<Vec<WorktreeMapping>> {
		let mut statement = self.connection.prepare(
			"
			SELECT project_id, issue_id, branch_name, worktree_path
			FROM worktree_mappings
			WHERE project_id = ?1
			ORDER BY issue_id
			",
		)?;
		let rows = statement.query_map(params![project_id], |row| {
			Ok(WorktreeMapping {
				project_id: row.get(0)?,
				issue_id: row.get(1)?,
				branch_name: row.get(2)?,
				worktree_path: PathBuf::from(row.get::<_, String>(3)?),
			})
		})?;

		let mappings = rows.collect::<rusqlite::Result<Vec<_>>>()?;

		Ok(mappings)
	}

	/// Remove the worktree mapping for one issue.
	pub fn clear_worktree(&self, issue_id: &str) -> Result<()> {
		self.connection
			.execute("DELETE FROM worktree_mappings WHERE issue_id = ?1", params![issue_id])?;

		Ok(())
	}

	fn initialize(&self) -> Result<()> {
		self.connection.execute_batch(INIT_SQL)?;

		Ok(())
	}
}

/// Active lease for one issue.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Worktree mapping for one issue lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeMapping {
	project_id: String,
	issue_id: String,
	branch_name: String,
	worktree_path: PathBuf,
}
impl WorktreeMapping {
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

	/// Filesystem path to the worktree checkout.
	pub fn worktree_path(&self) -> &Path {
		&self.worktree_path
	}
}

#[cfg(test)]
mod tests {
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
	}

	#[test]
	fn manages_worktree_mappings() {
		let store = StateStore::open_in_memory().expect("in-memory state store should open");

		store
			.upsert_worktree("pubfi", "PUB-101", "x/pub-101", "/tmp/worktrees/pub-101")
			.expect("worktree mapping should be recorded");

		let mapping = store
			.worktree_for_issue("PUB-101")
			.expect("mapping lookup should succeed")
			.expect("mapping should exist");

		assert_eq!(mapping.issue_id(), "PUB-101");
		assert_eq!(mapping.branch_name(), "x/pub-101");
		assert_eq!(mapping.worktree_path(), Path::new("/tmp/worktrees/pub-101"));
		assert_eq!(mapping.project_id(), "pubfi");
		assert_eq!(store.list_worktrees("pubfi").expect("list should succeed").len(), 1);
		store.clear_worktree("PUB-101").expect("mapping should be deleted");
		assert!(store.worktree_for_issue("PUB-101").expect("lookup should succeed").is_none());
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

	use std::path::Path;
}
