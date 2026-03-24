# Pilot Runbook

Goal: Run the `maestro` MVP against one configured Linear project and one target repository, with `maestro` itself as the default first pilot target.
Read this when: You are preparing a dry run or live self-dogfood pilot and need the bounded operator procedure for config, target-repo requirements, and expected run behavior.
Preconditions: `codex app-server` is available locally; `gh` is available locally for live PR-backed handoff validation; the target repository exists on disk with a root `WORKFLOW.md`; referenced `WORKFLOW.md [context.read_first]` files exist; the Linear team exposes the required workflow states; and the tracker and GitHub token env-var names are configured through `tracker.api_key_env_var` and `github.token_env_var` in `tmp/maestro.toml`.
Depends on: `docs/spec/system_maestro_runtime.md`, `docs/spec/system_workflow_contract.md`, `docs/spec/system_app_server_contract.md`, the target repository root `WORKFLOW.md`, and `Makefile.toml` for repo-native verification tasks.
Verification: `cargo run -- protocol probe`; `cargo run -- run --once --dry-run --config ./tmp/maestro.toml`; and, when the environment is ready, `cargo run -- run --once --config ./tmp/maestro.toml`.

## Alignment note

- Normal-path tracker writes now belong to the coding agent through issue-scoped tools.
- `maestro` still owns startup reconciliation, local leases, workspace lifecycle, retries, and fallback tracker writes when a run never reaches the normal agent-owned path.
- Every live pass now starts with reconciliation of stale local leases and terminal workspace mappings before issue selection.

## Preconditions

- `codex app-server` is available locally.
- `gh` is available locally for live runs that must validate PR-backed review handoff.
- The target repository already exists on disk as a normal Git checkout.
- The target repository has a root `WORKFLOW.md`.
- The target repository files referenced by `WORKFLOW.md [context.read_first]` exist.
- The Linear team already has the workflow states used by the target `WORKFLOW.md`.
- The Linear API token env-var name is configured through `tracker.api_key_env_var` in `tmp/maestro.toml`.
- GitHub auth for review handoff and post-review status is configured through `github.token_env_var` in `tmp/maestro.toml`; `maestro` does not fall back to ambient `GH_TOKEN` or an existing `gh auth login` session.

Recommended first-run check:

```sh
cargo run -- protocol probe
```

If `protocol probe` does not return `PROBE_OK`, stop there. The orchestrator loop depends on the same direct `app-server` contract.

## Recommended layout

For the recommended first deployment, keep the live local config at `tmp/maestro.toml` and point it back at this repository. If you need a checked-in template, copy `maestro.example.toml` first.

```text
/path/to/hack-ink/maestro/
  maestro.example.toml
  tmp/maestro.toml

/path/to/hack-ink/maestro/
  WORKFLOW.md

/path/to/hack-ink/maestro/.workspaces/
  XY-123/
  XY-124/
```

`maestro` resolves config in this order:

1. `--config <PATH>`
2. `./tmp/maestro.toml`
3. The platform default config path returned by `directories::ProjectDirs`

Runtime state now lives in process memory only. On restart, `maestro` rebuilds retained workspace knowledge and active-lane recovery intent from current Linear issue state plus deterministic `.workspaces/<ISSUE>` inspection.

That recovery is still scoped by configured `id`, so reconciliation and cleanup operate within the single configured project lane for this `tmp/maestro.toml`.

## Sample service config

```toml
id = "maestro"
repo_root = "/absolute/path/to/hack-ink/maestro"
workspace_root = "/absolute/path/to/hack-ink/maestro/.workspaces"
workflow_path = "WORKFLOW.md"

[tracker]
project_slug = "1a216b6d7100"
api_key_env_var = "LINEAR_API_KEY"

[github]
token_env_var = "GITHUB_TOKEN"

[agent]
transport = "stdio://"
```

Notes:

- `repo_root` should point at this repository for the first self-dogfood pilot.
- `workspace_root` is where `maestro` creates per-issue clone-backed workspaces. For the first pilot, use a repo-local path such as `.workspaces`.
- `workflow_path` is repository-relative and defaults to `WORKFLOW.md`.
- `transport` is optional and defaults to `stdio://`.
- Maestro does not expose repo-local model or reasoning overrides. `codex app-server` inherits those defaults from `~/.codex/config.toml`.
- `api_key_env_var` is required and must name the environment variable that stores the Linear API token.
- `github.token_env_var` is required for PR-backed review handoff validation and post-review PR-state inspection and must name the environment variable that stores the GitHub token.
- The recommended current tracker scope is the bounded `Maestro Pilot Ops Hardening` project in hackink Linear.
- Checked-in config examples should use the canonical Linear `slugId` for the target project. For the current self-dogfood pilot, that value is `1a216b6d7100`.

## Target repository contract

The downstream repository must provide a parseable root `WORKFLOW.md` with TOML frontmatter. For the MVP, the frontmatter contract lives in [`docs/spec/system_workflow_contract.md`](../spec/system_workflow_contract.md). For the first pilot, that means this repository's own root [`WORKFLOW.md`](../../WORKFLOW.md).

At minimum, the target repo should define:

- `[tracker] provider = "linear"`
- `[tracker] project_slug = "<Linear project slugId>"`
- `[tracker] startable_states = ["Todo"]` or another explicit start set
- `[agent]` policy such as sandbox and approval mode
- `[execution] max_attempts`
- `[execution] max_retry_backoff_ms`
- optional `[context] read_first = [...]` only when the repo truly needs extra repo-local files loaded in addition to the `WORKFLOW.md` body; treat this as a Maestro-local extension, not as the primary policy surface

The target Linear team should also expose:

- startable states such as `Todo`
- handoff state such as `In Review`
- terminal states such as `Done`, `Canceled`, and `Duplicate`
- optional label `maestro:manual-only` to opt out of automation
- optional label `maestro:needs-attention` for retry-exhausted or human-required failures

If `maestro:needs-attention` does not exist, the run will still fail correctly. `maestro` will log a warning, explain the missing label in the failure comment, and keep the issue in a non-startable guard state instead of allowing another automatic retry from `Todo`.

## Recommended first scope

Use `maestro` itself as the first target repo and keep the tracker scope bounded to the `Maestro Pilot Ops Hardening` project rather than a broad team backlog. That keeps the current dry run and live run inside one repo, one project, and one workspace root.

## Running the pilot

### Dry run

Use dry run first to validate config loading, issue discovery, and workspace planning without mutating Linear or creating workspace directories.

```sh
cargo run -- run --once --dry-run --config ./tmp/maestro.toml
```

Expected behavior:

- loads the configured project
- loads the target repo `WORKFLOW.md`
- queries Linear for the configured project slug
- applies the eligibility filter
- prints the selected issue, branch name, workspace path, and attempt number

If no config is found, the command exits cleanly with:

```text
dry run: no maestro config found; nothing to execute.
```

### Live run

```sh
cargo run -- run --once --config ./tmp/maestro.toml
```

On a normal successful run, `maestro` will:

1. reconcile stale leases and terminal workspace mappings for the configured project
2. select one eligible Linear issue
3. create or reuse a deterministic clone-backed workspace
4. refresh the issue once more before execution and skip the lane if it became terminal or otherwise ineligible
5. acquire a local lease
6. let the coding agent perform the normal-path `In Progress` transition and start comment through issue-scoped tools
7. run Codex through direct `app-server`
8. run the configured validation commands inside the workspace
9. require the coding agent to record a PR-backed review handoff and explicitly finalize the terminal path through the issue-scoped tool bridge
10. let `maestro` write the completion comment and `In Review` transition only after its own validation succeeds

Saved plan completion alone is not a successful lane exit. Even if coding work and repository checks are done, the turn is still incomplete until the agent records either the review-handoff path or the manual-attention path and then calls `issue_terminal_finalize` for that same path.

After `protocol probe`, `run --once --dry-run`, and `run --once` all behave as expected, use daemon mode for the long-running pilot loop:

```sh
cargo run -- daemon --poll-interval-s 60 --config ./tmp/maestro.toml
```

Daemon mode currently requires a Unix target because the parent process hands the single project dispatch-slot lock to the spawned `run --once` child via file-descriptor inheritance.

If you need remote read-only inspection, add an optional `[operator_http]` block to the same service config before starting the daemon:

```toml
[operator_http]
listen_address = "127.0.0.1:8900"
```

The listener is disabled by default when that block is absent. When enabled, daemon mode serves the same JSON operator snapshot used by `cargo run -- status --json --config ./tmp/maestro.toml` from `GET /state`.

During daemon mode, each poll tick now does two distinct things:

1. inspect any currently leased active lane
2. reconcile stale or terminal local state before selecting new work

Daemon mode also reloads the configured repo-owned `WORKFLOW.md` defensively on future ticks. A newly valid workflow document affects later dispatch, retry, post-exit reconciliation, and prompt generation without restarting the process. If the same configured path becomes invalid after a prior successful load, the daemon logs a warning and keeps the last known good workflow active; an already running child lane keeps the workflow snapshot it started with.

The active-lane reconciliation rules are:

- terminal issue: stop the lane, mark the run `terminated`, and remove the workspace
- non-terminal issue that has left both `In Progress` and any configured startable pre-claim state: stop the lane, mark the run `interrupted`, and keep the workspace
- issue still sitting in a startable state during early startup: leave it alone for that tick so the child can finish its initial tracker transition
- stalled lane with no app-server activity through the idle budget: stop the lane, mark the run `stalled`, and move the issue back through the human-attention failure path for manual repair
- child already exited before the next tick: still inspect persisted protocol activity so idle-timeout exits converge as `stalled`

## Workspace behavior

Each issue gets a deterministic lane:

- branch: `x/<project-id>-<issue-identifier>`
- path: `<workspace_root>/<ISSUE_IDENTIFIER>`

Example:

```text
branch  x/maestro-xy-123
path    /absolute/path/to/hack-ink/maestro/.workspaces/XY-123
```

Retries reuse the same workspace path.

If an issue becomes non-terminal but temporarily ineligible while the lane is being prepared, `maestro` skips execution for that pass and leaves the workspace in place for a later retry.

Each workspace is self-contained even when the visible directory lives under `.workspaces/<ISSUE>`. `maestro` clones the source repository into that lane, rewrites `origin` back to the source repository's remote, and refuses to continue if `git_dir` or `git_common_dir` resolves outside the lane root.

## Inspecting a failed run

Start with Linear:

- check the issue state
- read the latest `maestro` comment for `run_id`, attempt number, timestamps, and next action
- if retries were exhausted, look for the `maestro:needs-attention` label
- if the agent explicitly requested human attention, expect the issue to move back to `Todo` with `maestro:needs-attention` immediately instead of retrying
- any issue that still carries `maestro:needs-attention` is intentionally ineligible for another automatic run until a human clears that label
- if the failure comment says the label was unavailable on the team, expect the issue to remain in a non-startable guard state such as `In Progress` until a human moves it back to a startable state manually
- if the issue is already terminal, expect the workspace to disappear on the next live pass or startup reconciliation
- if the run failed as `stalled_run_detected`, expect the workspace to remain in place so you can inspect the partially completed lane before re-enabling automation

Then inspect the workspace mentioned in the comment:

```sh
git -C /absolute/path/to/hack-ink/maestro/.workspaces/XY-123 status --short
git -C /absolute/path/to/hack-ink/maestro/.workspaces/XY-123 log --oneline --decorate -5
```

Before dropping to local storage internals, inspect the supported runtime surface:

```sh
cargo run -- status --config ./tmp/maestro.toml
cargo run -- status --json --config ./tmp/maestro.toml
```

Use the human-readable view when you need the current leased run, retained workspace, and recent attempt summary at a glance. Use `--json` when you want a machine-readable snapshot with stable identifiers such as `run_id`, `issue_id`, `thread_id`, `branch`, and repository-relative `workspace_path`.

The operator snapshot also exposes coarse liveness semantics so you do not have to infer progress from workspace file churn alone:

- `phase = executing`: the lane is actively running
- `phase = waiting_continuation`: the worker ended cleanly at a turn boundary and Maestro may resume it
- `phase = retry_backoff`: the lane is not dead; Maestro has a queued retry and reports `retry_kind`, `wait_reason`, and `next_retry_at`
- `phase = stalled`: the lane crossed the app-server idle timeout and needs inspection

When present, compare `last_run_activity_at`, `last_protocol_activity_at`, and `idle_for_seconds` before assuming a lane is stuck. Quiet work with fresh activity is different from a stale lane with no recent protocol progress.

If you pass `--limit`, it only caps the recent-run section. Active runs remain uncapped in both the human-readable and JSON status views so the currently leased lanes stay visible.

There is no longer a supported SQLite fallback for normal recovery. If `status` is insufficient, use the tracker plus retained workspace lane directly:

```sh
gh issue view XY-123 --comments
git -C /absolute/path/to/hack-ink/maestro/.workspaces/XY-123 status --short
git -C /absolute/path/to/hack-ink/maestro/.workspaces/XY-123 log --oneline --decorate -5
```

Use tracker comments for run ids, attempts, and failure class; use the retained workspace when the failure happened inside `app-server` transport or thread lifecycle rather than during repo validation commands.

## Re-running after failure

- If the run is still retryable, leave the issue in `In Progress` and let `maestro` retry.
- If `execution.max_turns` is greater than `1`, one bounded worker may now reuse the same app-server thread for multiple turns before it yields.
- Retryable daemon retries now split into a short continuation retry after a clean nonterminal worker exit and a capped exponential failure backoff after an abnormal worker exit.
- If the run moved back to `Todo` with `maestro:needs-attention`, inspect the workspace, fix the blocking problem, clear `maestro:needs-attention`, and then move the issue back into a startable state for another automated attempt.
- If the issue should never be automated again, add `maestro:manual-only`.

## Verification commands

When changing `maestro` itself, keep the pilot path healthy with:

```sh
cargo run -- protocol probe
cargo run -- run --once --dry-run --config ./tmp/maestro.toml
cargo make fmt-check
cargo make lint
cargo make test
```
