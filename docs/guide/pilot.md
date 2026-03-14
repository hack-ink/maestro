# Pilot Runbook

Goal: Run the `maestro` MVP against one configured Linear project and one target repository, with `maestro` itself as the default first pilot target.
Read this when: You are preparing a dry run or live self-dogfood pilot and need the bounded operator procedure for config, target-repo requirements, and expected run behavior.
Preconditions: `codex app-server` is available locally; `gh` is available locally for live PR-backed handoff validation; the target repository exists on disk with a root `WORKFLOW.md`; referenced `WORKFLOW.md [context.read_first]` files exist; the Linear team exposes the required workflow states; and the tracker API token is configured through `tracker.api_key` in `maestro.toml`.
Depends on: `docs/spec/system_maestro_runtime.md`, `docs/spec/system_workflow_contract.md`, `docs/spec/system_app_server_contract.md`, the target repository root `WORKFLOW.md`, and `Makefile.toml` for repo-native verification tasks.
Verification: `cargo run -- protocol probe`; `cargo run -- run --once --dry-run --config ./maestro.toml`; and, when the environment is ready, `cargo run -- run --once --config ./maestro.toml`.

## Alignment note

- Normal-path tracker writes now belong to the coding agent through issue-scoped tools.
- `maestro` still owns startup reconciliation, local leases, worktree lifecycle, retries, and fallback tracker writes when a run never reaches the normal agent-owned path.
- Every live pass now starts with reconciliation of stale local leases and terminal worktree mappings before issue selection.

## Preconditions

- `codex app-server` is available locally.
- `gh` is available locally for live runs that must validate PR-backed review handoff.
- The target repository already exists on disk as a normal Git checkout.
- The target repository has a root `WORKFLOW.md`.
- The target repository files referenced by `WORKFLOW.md [context.read_first]` exist.
- The Linear team already has the workflow states used by the target `WORKFLOW.md`.
- The Linear API token is configured through `tracker.api_key` in `maestro.toml`.

Recommended first-run check:

```sh
cargo run -- protocol probe
```

If `protocol probe` does not return `PROBE_OK`, stop there. The orchestrator loop depends on the same direct `app-server` contract.

## Recommended layout

For the recommended first deployment, keep `maestro.toml` alongside the checked-out `maestro` repo and point it back at this repository. Keep issue worktrees under the repo-local `.worktrees/` directory.

```text
/path/to/maestro/
  maestro.toml

/path/to/maestro/
  AGENTS.md
  WORKFLOW.md

/path/to/maestro/.worktrees/
  PUB-600/
  PUB-601/
```

`maestro` resolves config in this order:

1. `--config <PATH>`
2. `./maestro.toml`
3. The platform default config path returned by `directories::ProjectDirs`

The SQLite operational state is stored separately from the target repo and uses the filename `maestro.sqlite3` under the platform data directory.

The local state is scoped by configured `id`, so reconciliation and cleanup operate within the single configured project lane for this `maestro.toml`.

## Sample service config

```toml
id = "maestro"
repo_root = "/absolute/path/to/helixbox/maestro"
workspace_root = "/absolute/path/to/helixbox/maestro/.worktrees"
workflow_path = "WORKFLOW.md"

[tracker]
project_slug = "maestro-pilot-ops-hardening-8c4cd895b10d"
api_key = "$HELIXBOX_LINEAR_API_KEY"

[agent]
transport = "stdio://"
model = "gpt-5-codex"
```

Notes:

- `repo_root` should point at this repository for the first self-dogfood pilot.
- `workspace_root` is where `maestro` creates per-issue `git worktree` lanes. For the first pilot, use a repo-local path such as `.worktrees`.
- `workflow_path` is repository-relative and defaults to `WORKFLOW.md`.
- `transport` is optional and defaults to `stdio://`.
- `model` is optional. If present, it is passed through to `app-server` and recorded in the run-start Linear comment.
- `api_key` accepts either a literal Linear token or an environment-variable reference in the form `$ENV_VAR`.
- The recommended current tracker scope is the bounded `Maestro Pilot Ops Hardening` project in helixbox Linear, whose current project slug is `maestro-pilot-ops-hardening-8c4cd895b10d`.

## Target repository contract

The downstream repository must provide a parseable root `WORKFLOW.md` with TOML frontmatter. For the MVP, the frontmatter contract lives in [`docs/spec/system_workflow_contract.md`](../spec/system_workflow_contract.md). For the first pilot, that means this repository's own root [`WORKFLOW.md`](../../WORKFLOW.md).

At minimum, the target repo should define:

- `[tracker] provider = "linear"`
- `[tracker] project_slug = "<Linear project slugId>"`
- `[tracker] startable_states = ["Todo"]` or another explicit start set
- `[agent]` policy such as sandbox and approval mode
- `[execution] max_attempts`
- `[context] read_first = ["AGENTS.md"]` if repo policy should be loaded into issue-scoped instructions without relying on raw workflow-body inlining

The target Linear team should also expose:

- startable states such as `Todo`
- handoff state such as `In Review`
- terminal states such as `Done`, `Canceled`, and `Duplicate`
- optional label `maestro:manual-only` to opt out of automation
- optional label `maestro:needs-attention` for retry-exhausted or human-required failures

If `maestro:needs-attention` does not exist, the run will still fail correctly, but `maestro` will only post the failure comment and log a warning instead of adding the label.

## Recommended first scope

Use `maestro` itself as the first target repo and keep the tracker scope bounded to the `Maestro Pilot Ops Hardening` project rather than a broad team backlog. That keeps the current dry run and live run inside one repo, one project, and one worktree root.

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
9. require the coding agent to record a PR-backed review handoff through the issue-scoped tool bridge
10. let `maestro` write the completion comment and `In Review` transition only after its own validation succeeds

After `protocol probe`, `run --once --dry-run`, and `run --once` all behave as expected, use daemon mode for the long-running pilot loop:

```sh
cargo run -- daemon --poll-interval-s 60 --config ./maestro.toml
```

During daemon mode, each poll tick now does two distinct things:

1. inspect any currently leased active lane
2. reconcile stale or terminal local state before selecting new work

The active-lane reconciliation rules are:

- terminal issue: stop the lane, mark the run `terminated`, and remove the worktree
- non-terminal issue that has left both `In Progress` and any configured startable pre-claim state: stop the lane, mark the run `interrupted`, and keep the worktree
- issue still sitting in a startable state during early startup: leave it alone for that tick so the child can finish its initial tracker transition
- stalled lane with no app-server activity through the idle budget: stop the lane, mark the run `stalled`, and move the issue back through the human-attention failure path for manual repair
- child already exited before the next tick: still inspect persisted protocol activity so idle-timeout exits converge as `stalled`

## Worktree behavior

Each issue gets a deterministic lane:

- branch: `x/<project-id>-<issue-identifier>`
- path: `<workspace_root>/<ISSUE_IDENTIFIER>`

Example:

```text
branch  x/maestro-pub-600
path    /absolute/path/to/helixbox/maestro/.worktrees/PUB-600
```

Retries reuse the same worktree path.

If an issue becomes non-terminal but temporarily ineligible while the lane is being prepared, `maestro` skips execution for that pass and leaves the worktree in place for a later retry.

## Inspecting a failed run

Start with Linear:

- check the issue state
- read the latest `maestro` comment for `run_id`, attempt number, timestamps, and next action
- if retries were exhausted, look for the `maestro:needs-attention` label
- if the agent explicitly requested human attention, expect the issue to move back to `Todo` with `maestro:needs-attention` immediately instead of retrying
- any issue that still carries `maestro:needs-attention` is intentionally ineligible for another automatic run until a human clears that label
- if the issue is already terminal, expect the worktree to disappear on the next live pass or startup reconciliation
- if the run failed as `stalled_run_detected`, expect the worktree to remain in place so you can inspect the partially completed lane before re-enabling automation

Then inspect the worktree mentioned in the comment:

```sh
git -C /absolute/path/to/helixbox/maestro/.worktrees/PUB-600 status --short
git -C /absolute/path/to/helixbox/maestro/.worktrees/PUB-600 log --oneline --decorate -5
```

Before dropping to local storage internals, inspect the supported runtime surface:

```sh
cargo run -- status --config ./maestro.toml
cargo run -- status --json --config ./maestro.toml
```

Use the human-readable view when you need the current leased run, retained worktree, and recent attempt summary at a glance. Use `--json` when you want a machine-readable snapshot with stable identifiers such as `run_id`, `issue_id`, `thread_id`, `branch`, and repository-relative `worktree_path`.

If you still need the thin storage internals for deep forensics, inspect the SQLite file directly as a fallback:

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
- If the run moved back to `Todo` with `maestro:needs-attention`, inspect the worktree, fix the blocking problem, clear `maestro:needs-attention`, and then move the issue back into a startable state for another automated attempt.
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
