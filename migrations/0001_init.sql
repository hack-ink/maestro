CREATE TABLE IF NOT EXISTS issue_leases (
    project_id TEXT NOT NULL,
    issue_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    acquired_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS run_attempts (
    run_id TEXT PRIMARY KEY,
    issue_id TEXT NOT NULL,
    attempt_number INTEGER NOT NULL,
    status TEXT NOT NULL,
    thread_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS event_journal (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    sequence_number INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(run_id, sequence_number)
);

CREATE TABLE IF NOT EXISTS workspace_mappings (
    project_id TEXT NOT NULL,
    issue_id TEXT PRIMARY KEY,
    branch_name TEXT NOT NULL,
    workspace_path TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
