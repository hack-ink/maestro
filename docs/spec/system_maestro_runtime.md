# Maestro Runtime Specification

Purpose: Define the authoritative runtime model for the `maestro` MVP.
Status: normative
Read this when: You need the authoritative model for issue eligibility, leases, lane ownership, runtime states, tracker-write ownership, or Linear writeback behavior.
Not this document: The low-level `app-server` protocol contract, the downstream `WORKFLOW.md` schema, or the operator pilot procedure.
Defines: The runtime scope, source-of-truth boundaries, eligibility rules, lane model, local state machine, tracker-write ownership, and writeback semantics.

## Scope

- One `maestro` service instance.
- One configured Linear project scope at a time.
- One isolated clone-backed workspace lane per eligible issue.
- One direct `codex app-server` session per run attempt.

## Upstream alignment

- Upstream Symphony is the architectural reference for scheduler and runner ownership.
- `maestro` keeps two deliberate divergences:
  - Rust implementation instead of Elixir
  - TOML frontmatter in `WORKFLOW.md` instead of YAML
- `maestro` should align with upstream on tracker ownership: the coding agent should normally perform issue-scoped tracker writes autonomously through runtime tools, while the service remains responsible for leases, workspace lifecycle, retries, reconciliation, and crash-safe fallback behavior.
- Current implementation note: normal-path tracker writes now flow through the issue-scoped tool bridge. Service-owned tracker writes remain only as fallback for reconciliation, crash recovery, and terminal failure handling.

## Source of truth boundaries

- Linear is the source of truth for issue lifecycle and coarse run outcomes.
- `maestro` local state is the source of truth only for active leases, in-flight protocol sessions, in-memory retry bookkeeping, and short-lived diagnostic history.
- `maestro` must not create a second long-lived business workflow model outside Linear.

## Runtime tuning inputs

- Runtime policy decisions that depend on Codex behavior, such as idle timeout, stall thresholds, retry cutoffs, or liveness heuristics, must not be tuned from local Maestro observation alone.
- For those decisions, use three inputs together:
  - the generated `codex app-server` schema for protocol shape
  - live pilot telemetry for observed event cadence and failure modes
  - the relevant Codex or `app-server` implementation path for terminal semantics, waiting states, and progress signals
- If those inputs disagree, treat the local implementation and generated schema as more authoritative than stale design assumptions.
- Do not hardcode a wall-clock budget only because one pilot run happened to exceed or fit within it. Timeout and stall policy should be grounded in upstream runtime behavior first, then tightened with local evidence.

## Core terms

- Issue: One tracker work item from the configured Linear project scope.
- Eligible issue: An issue that currently satisfies the `eligibility` rule in this specification.
- Lease: A local guarantee that only one active `maestro` run is processing a given issue.
- Run attempt: One bounded orchestration pass for one issue.
- Lane: The branch plus clone-backed workspace checkout associated with one issue.
- Terminal tracker state: A state that should not be auto-started by `maestro`. The default set is `Done`, `Canceled`, and `Duplicate`.

## Eligibility

An issue is eligible only when all of the following are true:

1. The issue belongs to the configured Linear project.
2. The issue state is in the configured `startable_states`.
3. The issue state is not in the configured terminal states.
4. The issue does not have the opt-out label `maestro:manual-only`.
5. The issue does not have the human-attention label `maestro:needs-attention`.
6. If the issue state is `Todo`, every blocker is already in a configured terminal state.
7. The issue does not already have an active `maestro` lease.
8. The project still has an available dispatch slot.

Default `startable_states`:

- `Todo`

Optional future expansion:

- `Backlog`

`In Progress` is not eligible by default. `maestro` should not race human-owned work that is already in progress.

Current runtime note:

- The current hack-ink `maestro` runtime is a single-worker model, so project-level concurrency is one dispatch slot.
- Active leases are the project-local claim set for that slot until a broader concurrency model lands.

## Lane model

- One eligible issue maps to one branch and one clone-backed workspace.
- One active run attempt owns the lane at a time.
- The lane path must be deterministic from issue identity so retries reuse the same checkout.
- The visible lane path may still live under a repo-local directory such as `.workspaces/<ISSUE>`, but the backing checkout must be self-contained and keep both `git_dir` and `git_common_dir` inside the lane.
- Before starting a live run, `maestro` must reject any prepared lane whose Git metadata escapes the writable workspace boundary.
- Workspace mappings and active leases must remain scoped to the configured `maestro.toml` `id` so reconciliation does not cross project boundaries.

## Runtime state machine

The runtime state machine is local to `maestro`. It is not a replacement for Linear workflow states.

| State | Meaning | Exit conditions |
| --- | --- | --- |
| `discovered` | The issue was listed from Linear and passed the eligibility filter. | Acquire lease or skip on conflict. |
| `leased` | `maestro` created the local lease and reserved the issue for one attempt. | Workspace bootstrap starts or lease fails. |
| `workspace_ready` | The issue lane exists locally and is ready for execution. | `app-server` session starts. |
| `running` | `maestro` has an active `app-server` thread and turn for the issue. | Turn completes, transport fails, or policy violation occurs. |
| `validating` | Agent execution finished and post-run validation commands are running. | Validation passes or fails. |
| `retry_wait` | The daemon is holding a queued retry entry for the leased lane after a clean continuation exit or a failure with remaining retry budget. | The queued retry revalidates and starts, the queued issue becomes non-active and the claim is released, or operator intervention cancels retries. |
| `needs_attention` | Retry budget is exhausted or human intervention is required. | Human updates the issue and it becomes eligible again. |
| `succeeded` | The attempt finished, validations passed, and the success writeback was committed to Linear. | Local cleanup begins. |
| `closed` | Local cleanup finished and the lease is gone. | None. |

After the `app-server` turn completes, `maestro` must resolve exactly one completion disposition before deciding whether the lane enters `validating`, `needs_attention`, or a retry path:

- `review_handoff`
  - The agent recorded a valid PR-backed review handoff and did not request human attention.
  - `maestro` proceeds into `validating`, then applies the success writeback if validation passes.
- `manual_attention`
  - The agent explicitly requested human attention by adding `maestro:needs-attention` and did not also record review handoff.
  - `maestro` skips success writeback and post-run validation commands, then enters the human-required failure flow immediately.
- invalid completion signaling
  - If the turn records both signals or neither signal, the attempt is invalid and must fail rather than guessing a completion path.

## Tracker write ownership

- Preferred steady state: the coding agent writes tracker state transitions, comments, and handoff data for the currently leased issue through issue-scoped runtime tools.
- Service-owned tracker writes are reserved for:
  - startup reconciliation
  - crash recovery
  - terminal fallback when the agent never reached the point of writing the tracker
- The service must never grant the coding agent broad tracker write access outside the currently leased issue.
- Before starting a live run, the service must reconcile stale local leases and any terminal workspace mappings against current tracker state.
- Before starting a live run, the service must fail fast if the local `gh` CLI needed for PR-backed review handoff inspection is unavailable.

## Linear writeback model

### Start writeback

At the start of a normal run, the coding agent should:

1. Acquire the local lease.
2. Transition the issue to `In Progress`.
3. Post a structured run-start comment.

Required run-start comment fields:

- `run_id`
- `attempt`
- `started_at`
- `workspace_path` as a repository-relative lane path such as `.workspaces/PUB-606`
- `transport`
- `model` when configured

### Completion disposition

Before applying success or failure writeback, `maestro` must classify the finished turn into one and only one terminal completion disposition:

| Disposition | Required agent signal | Forbidden co-signal | Runtime effect |
| --- | --- | --- | --- |
| `review_handoff` | `issue_review_handoff` plus `issue_terminal_finalize(path = "review_handoff")` | `maestro:needs-attention` | Run validation commands, revalidate PR state, post completion comment, transition to `In Review`. |
| `manual_attention` | `maestro:needs-attention` plus an explanatory issue comment, then `issue_terminal_finalize(path = "manual_attention")` | `issue_review_handoff` | Skip PR-backed success writeback and validation commands, then treat the run as a human-required failure immediately. |

If neither signal exists, or both signals exist, `maestro` must fail the attempt instead of inferring operator intent.
If the label is recorded without the required explanatory comment, `maestro` must also fail the attempt instead of treating it as a valid `manual_attention` exit.
If the resolved terminal path is not explicitly finalized through `issue_terminal_finalize`, the app-server wrapper must fail the turn before `maestro` records the attempt as successful.
The explanatory comment for `manual_attention` must describe the exact observed blocker and should include the failed command plus raw error text when available instead of speculating about unverified capability limits.
Saved plan completion, including `phase = "done"`, is never a substitute for the explicit terminal-finalization call.

### Success writeback

This path applies only when the resolved completion disposition is `review_handoff`.

During the run, the coding agent should prepare a PR-backed handoff by:

1. pushing the lane branch
2. creating or updating a non-draft PR for that branch
3. calling the dedicated review handoff tool with the PR URL and a short summary
4. calling `issue_terminal_finalize(path = "review_handoff")`

After agent execution and post-run validation succeed, `maestro` should:

1. confirm that the recorded PR still belongs to the current repository and branch and that its head commit matches the validated lane HEAD
2. transition the issue to `In Review`
3. post the structured completion comment from the recorded handoff

If the `In Review` transition succeeds but the completion comment fails, `maestro` must stop automatic retries for that attempt and converge the lane through the human-required failure path instead of treating it as retryable work.

Required completion comment fields:

- `run_id`
- `attempt`
- `finished_at`
- `branch`
- `pr_url`
- `workspace_path` as a repository-relative lane path such as `.workspaces/PUB-606`
- `validation_result`
- `summary`

`In Review` is a PR-backed handoff state. Successful runs must not auto-transition directly to `Done`, and generic issue transitions must not move straight into the success state without the recorded PR handoff.

### Failure writeback

This path applies to retryable failures, retry exhaustion, and explicit `manual_attention` exits.

Retryable failures with remaining budget:

- Keep the issue in `In Progress`, typically through an agent-authored retry comment.
- Queue the retry in daemon memory rather than immediately redispatching inside the same poll tick.
- Clean worker exits schedule a short continuation retry.
- Abnormal worker exits schedule exponential backoff capped by `execution.max_retry_backoff_ms`.
- When the queued issue disappears, reaches a terminal state, or otherwise becomes non-active before the retry fires, release the queued claim instead of redispatching it.

Retry-exhausted or human-required failures:

1. Transition the issue to `Todo`.
2. Add the label `maestro:needs-attention`.
3. Post a structured failure comment.
4. Finalize the terminal path with `issue_terminal_finalize(path = "manual_attention")`.

If the coding agent explicitly requests human attention by adding `maestro:needs-attention`, `maestro` must stop automatic retries for that attempt, skip PR-backed success writeback, and treat the lane as a human-required failure immediately.

If the configured `maestro:needs-attention` label is unavailable on the team and the configured failure state is startable, `maestro` must still block automatic reselection by leaving the issue in a non-startable guard state such as `In Progress`. In that case the failure comment must explain that the label could not be applied and that a human must move the issue back to a startable state manually after repair. Restart recovery must preserve that guard by writing a retained-workspace marker under `.workspaces/<ISSUE>/.maestro-terminal-guarded` and consulting it before redispatching recovered `In Progress` lanes.

Any issue carrying `maestro:needs-attention` is ineligible for another automatic run until a human clears the label and returns the issue to a startable state.

Required failure comment fields:

- `run_id`
- `attempt`
- `failed_at`
- `error_class`
- `next_action`
- `workspace_path` as a repository-relative lane path such as `.workspaces/PUB-606`

## Local operational state

The local runtime store may keep only the data needed to operate safely during the current process lifetime:

- issue leases
- run attempt identifiers
- thread or session identifiers
- protocol event journals
- workspace mappings
- tracker-write fallback metadata when the service must repair state after an interrupted run

This runtime state is process-memory only. `maestro` must not require a durable local database file for normal operation or restart recovery.
The local runtime store must not become the operator-facing source of workflow truth.
For daemon-child supervision, the active lane may also carry a short-lived workspace heartbeat marker at `.workspaces/<ISSUE>/.maestro-run-activity`. That marker is advisory, keyed to the current `run_id` plus `attempt`, and exists so the daemon can observe child activity across process boundaries, reconstruct a still-live retained lane after parent restart, and preserve retry-budget accounting without reviving a durable local state database.
For live execution, the current single project dispatch slot must still remain mutually exclusive across concurrent `maestro` processes. The runtime may enforce that exclusion with a short-lived workspace-root lock anchor, and daemon parents may hand that guard to the spawned `run --once` child so the active lane keeps exclusive ownership even if the parent restarts. That handoff currently requires Unix file-descriptor inheritance, so daemon mode is a Unix-only operator surface in this phase. Restart recovery must not depend on any durable lease database.

## Supported operator visibility surface

`maestro` must expose a supported local visibility surface for current runtime state without requiring operators to read source code or write ad hoc SQL.

The minimum supported surface is:

- structured runtime logs with stable identifiers such as `project_id`, `issue_id`, `issue`, `run_id`, `attempt`, `branch`, and repository-relative `workspace_path`
- a local status command that renders the current project-scoped snapshot in both human-readable and JSON forms

The status surface should describe only current local execution state, plus restart recovery synthesized from current tracker state and retained `.workspaces` lanes, for example:

- active leased runs
- recent run attempts with local status, thread id, and latest recorded protocol event
- retained workspace mappings

After a process restart, recent-run history may be shallow because attempt and event journals are memory-only. Operators should rely on `status`, tracker comments, and retained workspaces rather than local SQL for first-line recovery.

## Retention and cleanup

- Lease and session mappings: remove when the run closes.
- Attempt and event journals: retain only for the current process lifetime.
- Workspaces: retain while the issue is non-terminal.
- Terminal issue cleanup: once the issue reaches a terminal tracker state, remove the workspace during reconciliation or startup cleanup.
- If an issue becomes non-terminal but no longer eligible while `maestro` is still preparing the lane, keep the workspace and skip execution for that pass.

## Recovery rules

- On service startup, `maestro` must inspect the configured Linear project together with deterministic `.workspaces/<ISSUE>` paths to rebuild retained workspace mappings before starting new work.
- If Linear still shows a non-terminal `In Progress` issue and its retained workspace exists locally, `maestro` must treat that lane as a retry-style recovery candidate before selecting fresh `Todo` work.
- Retry recovery must treat a retained issue as belonging to the configured project when the tracker payload reports either the configured `project_slug` exactly or the trailing Linear `slugId` variant of that configured value.
- While daemon mode is running an active lane, every poll tick must refresh tracker state for the leased issue before considering any new selection.
- While daemon mode is running an active lane, that child must keep the workflow snapshot it started with; repo-owned `WORKFLOW.md` reloads affect later decisions without restarting the in-flight child.
- While daemon mode is supervising an active child process, stall detection must consult the child-updated `.maestro-run-activity` marker for the current `run_id` plus `attempt` instead of trusting only the daemon's process-local in-memory journal.
- While daemon mode owns a queued retry entry, that queued claim must take priority over normal candidate selection in the current single-slot runtime.
- While daemon mode is idle between lanes, it may reload the configured repo-owned `WORKFLOW.md` on each tick and immediately apply a newly valid document to future dispatch, retry, post-exit reconciliation, and prompt generation.
- If that same configured `WORKFLOW.md` path becomes invalid after a successful load, daemon mode must log the reload failure and keep the last known good document active instead of dropping the tick or clearing runtime policy.
- If the leased issue becomes terminal during a daemon tick, `maestro` must stop the active run, mark the attempt `terminated`, clear the lease, and clean the workspace.
- If the leased issue becomes non-terminal and leaves both the `In Progress` lane state and any configured startable pre-claim state, `maestro` must stop the active run, mark the attempt `interrupted`, clear the lease, and keep the workspace for inspection.
- A leased issue that is still in a configured startable state during early daemon ticks must be treated as a lane that has not finished claiming tracker ownership yet, not as an immediate non-active interruption.
- If a running attempt exceeds the app-server idle timeout with no recorded protocol activity, `maestro` must treat it as stalled, stop the active run, mark the attempt `stalled`, and converge the issue through the human-required failure path instead of silently retrying in this phase.
- If the supervised child already exited before the next daemon tick, stalled reconciliation must still inspect the just-finished lane using recorded protocol activity rather than skipping directly to generic failure handling.
- Reconciliation must mark locally active run attempts as `interrupted` when their stale lease is cleared, or `terminated` when the tracker issue is already terminal.
- Reconciliation must clear stale leases before the next issue-selection pass.
- When a queued retry becomes due, `maestro` must refresh that exact issue, redispatch it only if it is still active under retry policy, and otherwise release the queued claim.
- Before a prepared lane starts `app-server`, `maestro` must refresh the selected issue once more and skip execution if the issue became terminal or otherwise ineligible.
- If the local process crashed during a run, `maestro` must recover from current tracker state plus retained workspace inspection rather than assuming a durable lease/session database still exists.
- If Linear shows a non-terminal state but no local lease exists, the issue may become eligible again after reconciliation or may be redispatched through the retained recovered workspace.
