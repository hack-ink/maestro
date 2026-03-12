# Maestro Runtime Specification

Purpose: Define the authoritative runtime model for the `maestro` MVP.
Status: normative
Read this when: You need the authoritative model for issue eligibility, leases, lane ownership, runtime states, tracker-write ownership, or Linear writeback behavior.
Not this document: The low-level `app-server` protocol contract, the downstream `WORKFLOW.md` schema, or the operator pilot procedure.
Defines: The runtime scope, source-of-truth boundaries, eligibility rules, lane model, local state machine, tracker-write ownership, and writeback semantics.

## Scope

- One `maestro` service instance.
- One configured Linear project scope at a time.
- One isolated `git worktree` lane per eligible issue.
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
- `maestro` local state is the source of truth only for active leases, in-flight protocol sessions, retry bookkeeping, and short-lived diagnostic history.
- `maestro` must not create a second long-lived business workflow model outside Linear.

## Core terms

- Issue: One tracker work item from the configured Linear project scope.
- Eligible issue: An issue that currently satisfies the `eligibility` rule in this specification.
- Lease: A local guarantee that only one active `maestro` run is processing a given issue.
- Run attempt: One bounded orchestration pass for one issue.
- Lane: The branch plus `git worktree` checkout associated with one issue.
- Terminal tracker state: A state that should not be auto-started by `maestro`. The default set is `Done`, `Canceled`, and `Duplicate`.

## Eligibility

An issue is eligible only when all of the following are true:

1. The issue belongs to the configured Linear project.
2. The issue state is in the configured `startable_states`.
3. The issue state is not in the configured terminal states.
4. The issue does not have the opt-out label `maestro:manual-only`.
5. The issue does not already have an active `maestro` lease.

Default `startable_states`:

- `Todo`

Optional future expansion:

- `Backlog`

`In Progress` is not eligible by default. `maestro` should not race human-owned work that is already in progress.

## Lane model

- One eligible issue maps to one branch and one `git worktree`.
- One active run attempt owns the lane at a time.
- The lane path must be deterministic from issue identity so retries reuse the same checkout.
- Worktrees must be created and removed with `git worktree` commands, not manual directory copying or deletion.
- Worktree mappings and active leases must remain scoped to the configured `maestro.toml` project so reconciliation does not cross project boundaries.

## Runtime state machine

The runtime state machine is local to `maestro`. It is not a replacement for Linear workflow states.

| State | Meaning | Exit conditions |
| --- | --- | --- |
| `discovered` | The issue was listed from Linear and passed the eligibility filter. | Acquire lease or skip on conflict. |
| `leased` | `maestro` created the local lease and reserved the issue for one attempt. | Workspace bootstrap starts or lease fails. |
| `workspace_ready` | The issue lane exists locally and is ready for execution. | `app-server` session starts. |
| `running` | `maestro` has an active `app-server` thread and turn for the issue. | Turn completes, transport fails, or policy violation occurs. |
| `validating` | Agent execution finished and post-run validation commands are running. | Validation passes or fails. |
| `retry_wait` | The attempt failed but retry budget remains. | Retry delay expires and a new attempt starts, or operator intervention cancels retries. |
| `needs_attention` | Retry budget is exhausted or human intervention is required. | Human updates the issue and it becomes eligible again. |
| `succeeded` | The attempt finished, validations passed, and the success writeback was committed to Linear. | Local cleanup begins. |
| `closed` | Local cleanup finished and the lease is gone. | None. |

## Tracker write ownership

- Preferred steady state: the coding agent writes tracker state transitions, comments, and handoff data for the currently leased issue through issue-scoped runtime tools.
- Service-owned tracker writes are reserved for:
  - startup reconciliation
  - crash recovery
  - terminal fallback when the agent never reached the point of writing the tracker
- The service must never grant the coding agent broad tracker write access outside the currently leased issue.
- Before starting a live run, the service must reconcile stale local leases and any terminal worktree mappings against current tracker state.

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
- `worktree_path`
- `transport`
- `model` when configured

### Success writeback

After agent execution and post-run validation succeed, the coding agent should:

1. Transition the issue to `In Review`.
2. Post a structured completion comment.

Required completion comment fields:

- `run_id`
- `attempt`
- `finished_at`
- `branch`
- `worktree_path`
- `validation_result`
- `summary`

Successful runs must not auto-transition directly to `Done`.

### Failure writeback

Retryable failures with remaining budget:

- Keep the issue in `In Progress`, typically through an agent-authored retry comment.

Retry-exhausted or human-required failures:

1. Transition the issue to `Todo`.
2. Add the label `maestro:needs-attention`.
3. Post a structured failure comment.

Required failure comment fields:

- `run_id`
- `attempt`
- `failed_at`
- `error_class`
- `next_action`
- `worktree_path`

## Local operational state

The local persistence layer may store only the data needed to operate safely:

- issue leases
- run attempt identifiers
- thread or session identifiers
- protocol event journals
- worktree mappings
- tracker-write fallback metadata when the service must repair state after an interrupted run

The local persistence layer must not become the operator-facing source of workflow truth.

## Retention and cleanup

- Lease and session mappings: remove when the run closes.
- Attempt and event journals: retain for 14 days.
- Worktrees: retain while the issue is non-terminal.
- Terminal issue cleanup: once the issue reaches a terminal tracker state, remove the worktree during reconciliation or startup cleanup.
- If an issue becomes non-terminal but no longer eligible while `maestro` is still preparing the lane, keep the worktree and skip execution for that pass.

## Recovery rules

- On service startup, `maestro` must reconcile local leases against current Linear state before starting new work.
- Reconciliation must mark locally active run attempts as `interrupted` when their stale lease is cleared, or `terminated` when the tracker issue is already terminal.
- Reconciliation must clear stale leases before the next issue-selection pass.
- Before a prepared lane starts `app-server`, `maestro` must refresh the selected issue once more and skip execution if the issue became terminal or otherwise ineligible.
- If the local process crashed during a run, `maestro` may resume, retry, or mark the issue failed based on the retained lease, session, and attempt records.
- If Linear shows a non-terminal state but no local lease exists, the issue may become eligible again after reconciliation.
