+++
version = 1

[tracker]
provider = "linear"
project_slug = "maestro-mvp-10bbdae9b904"
startable_states = ["Todo"]
terminal_states = ["Done", "Canceled", "Duplicate"]
in_progress_state = "In Progress"
success_state = "In Review"
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

Keep changes scoped to the current issue. Do not widen scope into unrelated cleanup or parallel feature work.

When runtime behavior changes, update the relevant specs and operator docs in the same lane so the repo stays self-describing.

Use Linear as the internal execution tracker of record for this repository. Do not create or mirror GitHub issues for internal delivery tracking unless the user explicitly asks for public issue tracking.
