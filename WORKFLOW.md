+++
version = 1

[tracker]
provider = "linear"
project_slug = "1a216b6d7100"
startable_states = ["Todo"]
terminal_states = ["Done", "Canceled", "Duplicate"]
in_progress_state = "In Progress"
success_state = "In Review"
completed_state = "Done"
failure_state = "Todo"
opt_out_label = "maestro:manual-only"
needs_attention_label = "maestro:needs-attention"

[agent]
transport = "stdio://"
sandbox = "workspace-write"
approval_policy = "never"
personality = "pragmatic"

[execution]
max_attempts = 3
max_turns = 3
max_retry_backoff_ms = 300000
max_concurrent_agents = 1
max_concurrent_agents_by_state = { "In Progress" = 1 }
validation_commands = [
  "cargo make fmt-check",
  "cargo make lint",
  "cargo make test",
]

[context]
read_first = ["AGENTS.md", "docs/index.md"]
+++

Read `AGENTS.md` first.

Use `cargo make` whenever an equivalent task exists.

Use the issue-scoped tracker tools autonomously for normal-path state changes and comments on the currently leased issue.

Treat `In Review` as a PR-backed handoff state. A normal success path must push the lane branch, create or update a non-draft PR, and only then ask `maestro` to complete the `In Review` handoff.

Keep changes scoped to the current issue. Do not widen scope into unrelated cleanup or parallel feature work.

When runtime behavior changes, update the relevant specs and operator docs in the same lane so the repo stays self-describing.

Use Linear as the internal execution tracker of record for this repository. Do not create or mirror GitHub issues for internal delivery tracking unless the user explicitly asks for public issue tracking.
