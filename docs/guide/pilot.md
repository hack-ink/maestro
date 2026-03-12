# Pilot Runbook

Purpose: Run the `maestro` MVP against one configured Linear project and one target repository, with `maestro` itself as the default first pilot target.

## Alignment note

- Normal-path tracker writes now belong to the coding agent through issue-scoped tools.
- `maestro` still owns startup reconciliation, local leases, worktree lifecycle, retries, and fallback tracker writes when a run never reaches the normal agent-owned path.
- Every live pass now starts with reconciliation of stale local leases and terminal worktree mappings before issue selection.

## Preconditions

- `codex app-server` is available locally.
- The target repository already exists on disk as a normal Git checkout.
- The target repository has a root `WORKFLOW.md`.
- The target repository files referenced by `WORKFLOW.md [context.read_first]` exist.
- The Linear team already has the workflow states used by the target `WORKFLOW.md`.
- The Linear API token is available in the environment variable named by `projects.tracker.api_token_env`.

Recommended first-run check:

```sh
cargo run -- protocol probe
```

If `protocol probe` does not return `PROBE_OK`, stop there. The orchestrator loop depends on the same direct `app-server` contract.

## Recommended layout

For the recommended first deployment, keep `maestro.toml` alongside the checked-out `maestro` repo and point it back at this repository. Other deployment layouts are valid as long as the configured paths are explicit.

```text
/path/to/maestro/
  maestro.toml

/path/to/maestro/
  AGENTS.md
  WORKFLOW.md

/path/to/maestro-workspaces/maestro/
  PUB-600/
  PUB-601/
```

`maestro` resolves config in this order:

1. `--config <PATH>`
2. `./maestro.toml`
3. The platform default config path returned by `directories::ProjectDirs`

The SQLite operational state is stored separately from the target repo and uses the filename `maestro.sqlite3` under the platform data directory.

The local state is scoped by configured `projects.id`, so reconciliation and cleanup operate within one configured project lane at a time even when one service config contains multiple projects.

## Sample service config

```toml
[[projects]]
id = "maestro"
repo_root = "/absolute/path/to/helixbox/maestro"
workspace_root = "/absolute/path/to/maestro-workspaces/maestro"
workflow_path = "WORKFLOW.md"

[projects.tracker]
project_slug = "maestro-mvp-10bbdae9b904"
api_token_env = "LINEAR_API_KEY"

[projects.agent]
transport = "stdio://"
model = "gpt-5-codex"
```

Notes:

- `repo_root` should point at this repository for the first self-dogfood pilot.
- `workspace_root` is where `maestro` creates per-issue `git worktree` lanes.
- `workflow_path` is repository-relative and defaults to `WORKFLOW.md`.
- `transport` is optional and defaults to `stdio://`.
- `model` is optional. If present, it is passed through to `app-server` and recorded in the run-start Linear comment.
- The recommended first tracker scope is the bounded `Maestro MVP` project in helixbox Linear, whose current project slug is `maestro-mvp-10bbdae9b904`.

## Target repository contract

The downstream repository must provide a parseable root `WORKFLOW.md` with TOML frontmatter. For the MVP, the frontmatter contract lives in [`docs/spec/system_workflow_contract.md`](../spec/system_workflow_contract.md). For the first pilot, that means this repository's own root [`WORKFLOW.md`](../../WORKFLOW.md).

At minimum, the target repo should define:

- `[tracker] provider = "linear"`
- `[tracker] project_slug = "<Linear project slugId>"`
- `[tracker] startable_states = ["Todo"]` or another explicit start set
- `[agent]` policy such as sandbox and approval mode
- `[execution] max_attempts`
- `[context] read_first = ["AGENTS.md"]` if repo policy should be loaded before the workflow body

The target Linear team should also expose:

- startable states such as `Todo`
- handoff state such as `In Review`
- terminal states such as `Done`, `Canceled`, and `Duplicate`
- optional label `maestro:manual-only` to opt out of automation
- optional label `maestro:needs-attention` for retry-exhausted failures

If `maestro:needs-attention` does not exist, the run will still fail correctly, but `maestro` will only post the failure comment and log a warning instead of adding the label.

## Recommended first scope

Use `maestro` itself as the first target repo and keep the tracker scope bounded to the `Maestro MVP` project rather than a broad team backlog. That keeps the first dry run and first live run inside one repo, one project, and one worktree root.

## Running the pilot

### Dry run

Use dry run first to validate config loading, issue discovery, and workspace planning without mutating Linear or creating worktrees.

```sh
cargo run -- run --once --dry-run --config ./maestro.toml
```

Expected behavior:

- loads the configured project
- loads the target repo `WORKFLOW.md`
- queries Linear for the configured project slug
- applies the eligibility filter
- prints the selected issue, branch name, worktree path, and attempt number

If no config is found, the command exits cleanly with:

```text
dry run: no maestro config found; nothing to execute.
```

### Live run

```sh
cargo run -- run --once --config ./maestro.toml
```

On a normal successful run, `maestro` will:

1. reconcile stale leases and terminal worktree mappings for the configured project
2. select one eligible Linear issue
3. create or reuse a deterministic `git worktree`
4. refresh the issue once more before execution and skip the lane if it became terminal or otherwise ineligible
5. acquire a local lease
6. let the coding agent perform the normal-path `In Progress` transition and start comment through issue-scoped tools
7. run Codex through direct `app-server`
8. run the configured validation commands inside the worktree
9. let the coding agent perform the normal-path `In Review` transition and completion comment through issue-scoped tools

## Worktree behavior

Each issue gets a deterministic lane:

- branch: `x/<project-id>-<issue-identifier>`
- path: `<workspace_root>/<ISSUE_IDENTIFIER>`

Example:

```text
branch  x/maestro-pub-600
path    /absolute/path/to/maestro-workspaces/maestro/PUB-600
```

Retries reuse the same worktree path.

If an issue becomes non-terminal but temporarily ineligible while the lane is being prepared, `maestro` skips execution for that pass and leaves the worktree in place for a later retry.

## Inspecting a failed run

Start with Linear:

- check the issue state
- read the latest `maestro` comment for `run_id`, attempt number, timestamps, and next action
- if retries were exhausted, look for the `maestro:needs-attention` label
- if the issue is already terminal, expect the worktree to disappear on the next live pass or startup reconciliation

Then inspect the worktree mentioned in the comment:

```sh
git -C /absolute/path/to/maestro-workspaces/maestro/PUB-600 status --short
git -C /absolute/path/to/maestro-workspaces/maestro/PUB-600 log --oneline --decorate -5
```

If you need the thin operational state, inspect the SQLite file directly:

```sh
DB_PATH=/absolute/path/to/maestro.sqlite3
sqlite3 "$DB_PATH" 'select project_id, issue_id, run_id from issue_leases;'
sqlite3 "$DB_PATH" 'select run_id, issue_id, attempt_number, status, thread_id from run_attempts order by updated_at desc;'
sqlite3 "$DB_PATH" 'select run_id, sequence_number, event_type from event_journal order by id desc limit 50;'
sqlite3 "$DB_PATH" 'select project_id, issue_id, branch_name, worktree_path from worktree_mappings;'
```

Use the event journal when the failure happened inside `app-server` transport or thread lifecycle rather than during repo validation commands.

## Re-running after failure

- If the run is still retryable, leave the issue in `In Progress` and let `maestro` retry.
- If the run moved back to `Todo` with `maestro:needs-attention`, inspect the worktree, fix the blocking problem, and then move the issue back into a startable state for another automated attempt.
- If the issue should never be automated again, add `maestro:manual-only`.

## Verification commands

When changing `maestro` itself, keep the pilot path healthy with:

```sh
cargo run -- protocol probe
cargo run -- run --once --dry-run --config ./maestro.toml
cargo make fmt-check
cargo make lint
cargo make test
```
