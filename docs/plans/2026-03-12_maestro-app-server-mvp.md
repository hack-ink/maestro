# Maestro App-Server MVP Plan

## Goal

Build a standalone `maestro` MVP in this repository that can poll one configured Linear scope, select one eligible issue, create an isolated local workspace for the target repository, run Codex through the direct `app-server` protocol, and write the coarse-grained run outcome back to Linear while keeping only the thinnest local operational state needed for execution and crash recovery.

## Scope

- Implement the `maestro` control-plane MVP in Rust inside this repository.
- Use direct `codex app-server` integration for agent execution instead of an SDK bridge.
- Support one bounded pilot path, one target repository at a time, and one issue execution lane per eligible issue.
- Define a repo-local `WORKFLOW.md` contract that `maestro` reads from downstream target repositories.
- Keep Linear as the primary system of record for issue workflow and coarse run outcomes.
- Keep only minimal local operational state for leases, in-flight protocol sessions, and crash recovery.

## Non-goals

- Building the orchestration runtime inside `pubfi-mono` or any other target repository.
- Multi-node scheduling, SSH workers, or a remote control plane.
- A web dashboard, generalized plugin ecosystem, or merge automation in v1.
- Owning a second full workflow-state model outside Linear.
- An SDK-bridge execution path as the primary implementation.

## Constraints

- The repository is still on a placeholder scaffold today: [README.md](../../README.md), [src/main.rs](../../src/main.rs), and [src/cli.rs](../../src/cli.rs) must be normalized before the runtime is meaningful.
- Repo-native verification should continue to go through `cargo make` because [Makefile.toml](../../Makefile.toml) is the local source of truth for checks.
- `cargo make fmt-check` currently shells out to `cargo +nightly fmt --all -- --check`, so nightly `rustfmt` is an execution prerequisite even though [rust-toolchain.toml](../../rust-toolchain.toml) defaults to stable.
- The local Codex CLI already exposes `codex app-server` and `codex app-server generate-json-schema`, so the MVP can be grounded in the local protocol surface instead of reverse-engineering from prose docs alone.
- The MVP should prefer `stdio://` transport for `app-server` first. `ws://` can be added later if remote or browser-driven controllers become necessary.
- Workspace provisioning should use `git workspace` lifecycle operations rather than ad hoc cloned directories, and the service should follow the same branch-isolated lane model used by the local workspace workflow.

## Open Questions

- None.

## Execution State

- Last Updated: 2026-03-12
- Next Checkpoint: None. MVP plan complete.
- Blockers: None.

## Decision Notes

- Direct `app-server` integration is the chosen execution path for this plan; an SDK bridge is intentionally out of scope unless the `app-server` protocol proves insufficient.
- Linear is the primary system of record for issue lifecycle and coarse run outcomes. `maestro` should read and write Linear directly instead of asking operators to reconcile a parallel state model by hand.
- The orchestrator still needs a thin local operational state layer for leases, in-flight `app-server` sessions, and crash-safe retries, but that layer must not become a second long-lived business workflow model.
- For the local MVP, `app-server` transport should start over stdio because `codex app-server --listen stdio://` is already available locally and avoids adding a second network lifecycle before the first end-to-end loop works.
- `maestro` should integrate with Linear directly inside the service runtime rather than depending on the interactive Codex MCP tool surface that is available to the agent during development.
- Downstream `WORKFLOW.md` should use Markdown with TOML frontmatter so the contract stays human-readable while remaining natural to parse in Rust.
- Workspace provisioning should be built on `git workspace` from the start instead of one-fresh-clone-per-issue directories.
- `eligibility` should be status-first and conservative: an issue is eligible only when it belongs to the configured project, is in the configured startable states, is not in a terminal state, is not explicitly opted out of automation, and does not already have an active `maestro` run.
- Labels should remain auxiliary metadata rather than the primary workflow state machine. Use labels for opt-out, routing, or exceptional conditions, not for normal execution progression.
- The start writeback should be `In Progress` plus a structured comment. The success writeback should be `In Review` plus a structured comment. Do not auto-transition successful agent runs directly to `Done`.
- The default failure policy should be: while automatic retries are still active, keep the issue in `In Progress` and append failure/retry comments; once retry budget is exhausted or human intervention is required, move the issue back to `Todo`, add the label `maestro:needs-attention`, and post a structured failure comment.
- The standard opt-out label should be `maestro:manual-only`.
- Local operational-state retention should be thin and bounded: clear lease/session mappings when a run closes, retain attempt/event journals for 14 days, and keep workspaces only while the issue remains non-terminal; once the issue reaches a terminal tracker state, remove the workspace during reconciliation/startup cleanup.
- These workflow choices follow the Symphony-style handoff model plus issue-tracker best practices: keep the state machine small, represent active work in status, and use comments for auditable execution detail.
- The direct `app-server` client should filter notifications by the target `threadId` before classifying run outcomes or recording events because the local desktop transport can emit unrelated thread traffic on the same connection during development.

## Implementation Outline

The core implementation should be a Rust control plane with five stable seams: configuration loading, repo-local workflow parsing, Linear tracker access, workspace lifecycle management, and an `app-server` client that speaks JSON-RPC over the child process stdio transport. The local schema export already shows the important protocol objects for the MVP: `ThreadStartParams`, `TurnStartParams`, `TurnCompletedNotification`, and `ThreadStatusChangedNotification`. That is enough to model one deterministic run loop without inventing an abstraction layer first.

The runtime should treat a Linear issue as a leaseable work item while keeping Linear as the coarse-grained source of truth. `maestro` should keep only thin local operational state such as a lease table, in-flight run/session mapping, and protocol event journal so a crashed process can resume or safely mark a run as failed. The service should map one eligible issue to one isolated workspace path backed by `git workspace`, then pass `cwd`, `approvalPolicy`, `sandbox`, `developerInstructions`, and the user input payload into `app-server` using the downstream repository's `WORKFLOW.md` contract.

The first execution checkpoint should stop at a one-shot `run --once` path rather than a forever daemon. That keeps the initial surface reviewable: prove that `maestro` can discover exactly one issue via the status-first eligibility filter, build exactly one workspace-backed workspace, launch exactly one Codex thread/turn, observe completion notifications, write `In Progress` and `In Review` transitions plus structured comments back to Linear, and retain only the local data needed for debugging or retry safety. Once that works, the poll loop and reconciliation become incremental additions rather than guesses.

## Task 1: Normalize the repository scaffold for Maestro

**Owner**

Main executor

**Status**

done

**Outcome**

The crate, CLI, README, and runtime entrypoint stop using placeholder names and expose an explicit `maestro` identity plus a CLI shape that can host orchestrator subcommands cleanly.

**Files**

- Modify: `Cargo.toml`
- Modify: `README.md`
- Modify: `src/main.rs`
- Modify: `src/cli.rs`
- Create: `src/lib.rs`

**Changes**

1. Replace the placeholder crate metadata, homepage/repository fields, and log-directory naming with `maestro` values.
2. Move runtime initialization into `src/lib.rs` so `src/main.rs` becomes a thin bootstrap and `src/cli.rs` can evolve into meaningful subcommands such as `run`, `daemon`, and `protocol`.
3. Replace the placeholder CLI argument with a subcommand-oriented skeleton that matches the future control-plane surface.
4. Verification completed with `cargo run -- --help`, `cargo make fmt-check`, `cargo make lint`, and `cargo make test`.

**Verification**

- `cargo run -- --help`
- `cargo make fmt-check`
- `cargo make lint`
- `cargo make test`

**Dependencies**

- None.

## Task 2: Define the normative runtime and workflow contracts

**Owner**

Main executor

**Status**

done

**Outcome**

The repository has explicit specs for the orchestration state machine, the downstream `WORKFLOW.md` contract, the Linear writeback model, and the direct `app-server` integration boundary, so later implementation does not guess at behavior.

**Files**

- Modify: `docs/spec/index.md`
- Modify: `docs/index.md`
- Create: `docs/spec/system_maestro_runtime.md`
- Create: `docs/spec/system_workflow_contract.md`
- Create: `docs/spec/system_app_server_contract.md`

**Changes**

1. Define the `maestro` state machine for issue discovery, lease acquisition, workspace preparation, agent execution, reconciliation, retry, and terminal outcomes.
2. Specify the machine-readable portion of downstream `WORKFLOW.md` using TOML frontmatter, including keys for tracker scope, agent defaults, validation commands, and execution policy.
3. Document the Linear source-of-truth model, including the default eligibility rule (`project + startable states + no active run + no opt-out`) and the status-first philosophy that keeps labels auxiliary.
4. Document the direct `app-server` contract for MVP execution: thread creation, turn submission, stdio JSON-RPC framing, completion/error notifications, and which protocol fields `maestro` owns versus which come from repo workflow policy.
5. Document the default writeback policy: on start, transition to `In Progress` and post a structured run-start comment; on success, transition to `In Review` and post a structured completion comment; on retry-exhausted failure, transition to `Todo`, add `maestro:needs-attention`, and post a structured failure comment.
6. Document the standard automation-control labels, including `maestro:manual-only` for opt-out and `maestro:needs-attention` for failed runs requiring human follow-up.
7. Document the retention policy for thin local operational state, including lease/session cleanup, 14-day journal retention, and workspace cleanup once issues reach terminal tracker states.
8. Verification completed with `rg -n "system_maestro_runtime|system_workflow_contract|system_app_server_contract" docs/spec/index.md docs/index.md` and `cargo make fmt-check`.

**Verification**

- `rg -n "system_maestro_runtime|system_workflow_contract|system_app_server_contract" docs/spec/index.md docs/index.md`
- `cargo make fmt-check`

**Dependencies**

- Task 1.

## Task 3: Implement config loading, workflow parsing, and durable local state

**Owner**

Main executor

**Status**

done

**Outcome**

`maestro` can load service configuration, parse a downstream repository `WORKFLOW.md`, initialize local persistence, and store only the operational state needed to track leases and in-flight runs across process restarts.

**Files**

- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Modify: `src/cli.rs`
- Create: `src/config.rs`
- Create: `src/workflow.rs`
- Create: `src/state.rs`
- Create: `migrations/0001_init.sql`

**Changes**

1. Add the runtime dependencies needed for structured config, Markdown plus TOML-frontmatter parsing, and SQLite-backed persistence.
2. Implement a typed service config for target repositories, workspace roots, tracker credentials, and agent defaults.
3. Implement `WORKFLOW.md` parsing and validation against the Task 2 contract.
4. Add SQLite initialization and repositories for issue leases, in-flight run/session mappings, protocol event journals, and workspace mappings, while keeping authoritative business outcomes in Linear.
5. Verification completed with `cargo make fmt-check`, `cargo make lint`, and `cargo make test`.

**Verification**

- `cargo make fmt-check`
- `cargo make lint`
- `cargo make test`

**Dependencies**

- Task 2.

## Task 4: Implement the direct app-server client and run recorder

**Owner**

Main executor

**Status**

done

**Outcome**

`maestro` can spawn `codex app-server`, start a thread, send one turn, consume protocol notifications, and classify the final run outcome while recording the local protocol trail needed for debugging and retry safety.

**Files**

- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Modify: `src/cli.rs`
- Create: `src/agent/mod.rs`
- Create: `src/agent/app_server.rs`
- Create: `src/agent/json_rpc.rs`

**Changes**

1. Spawn `codex app-server --listen stdio://` as a managed child process and implement JSON-RPC request/response framing over stdin/stdout.
2. Map `maestro` run inputs into `ThreadStartParams` and `TurnStartParams`, including `cwd`, `approvalPolicy`, `sandbox`, `developerInstructions`, and user input derived from the issue plus `WORKFLOW.md`.
3. Record protocol notifications such as `ThreadStatusChanged`, `TurnStarted`, `TurnCompleted`, command output deltas, and terminal errors into the local event journal, while filtering out unrelated thread traffic on the shared local desktop connection.
4. Expose a `protocol probe` CLI path that validates the local `app-server` contract before the full orchestrator loop depends on it, using a constrained `PROBE_OK` turn plus an in-memory run journal.
5. Verification completed with `codex app-server generate-json-schema --out /tmp/maestro-app-server-schema-check`, `cargo run -- protocol probe`, `cargo make fmt-check`, `cargo make lint`, and `cargo make test`.

**Verification**

- `codex app-server generate-json-schema --out /tmp/maestro-app-server-schema-check`
- `cargo run -- protocol probe`
- `cargo make fmt-check`
- `cargo make lint`
- `cargo make test`

**Dependencies**

- Task 3.

## Task 5: Implement Linear polling, workspace bootstrap, and the one-shot orchestrator loop

**Owner**

Main executor

**Status**

done

**Outcome**

`maestro run --once` can discover one eligible issue from a configured Linear scope, acquire a local lease, prepare the target repository workspace using `git workspace`, run the direct `app-server` execution path, and write the final coarse outcome back to Linear.

**Files**

- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Modify: `src/cli.rs`
- Create: `src/tracker/mod.rs`
- Create: `src/tracker/linear.rs`
- Create: `src/workspace.rs`
- Create: `src/orchestrator.rs`

**Changes**

1. Implement direct Linear client reads and writes for the chosen pilot scope, including issue listing, eligibility filtering, and the agreed in-progress and terminal writeback actions.
2. Implement workspace provisioning that maps issue identity to a deterministic local path and bootstraps the target repository checkout through `git workspace` lifecycle operations.
3. Add lease acquisition, run attempt creation, and orchestration glue that ties Linear issue data, `WORKFLOW.md`, workspace setup, and `app-server` execution into one `run --once` command.
4. Implement the default status-first writeback flow: move the issue to `In Progress` and post a run-start comment before execution, then move it to `In Review` and post a run-completion comment on success.
5. Implement the default failure flow: keep retrying runs in `In Progress`, and when retry budget is exhausted move the issue to `Todo`, add `maestro:needs-attention`, and post a structured failure comment.
6. Retain only the local retry/debug data needed for operators to diagnose failures, while leaving the authoritative coarse outcome in Linear and pruning local state according to the plan's retention defaults.
7. Verification completed with `cargo run -- run --once --dry-run`, `cargo make fmt-check`, `cargo make lint`, `cargo make test`, and `git diff --check`.

**Verification**

- `cargo run -- run --once --dry-run`
- `cargo make fmt-check`
- `cargo make lint`
- `cargo make test`
- `git diff --check`

**Dependencies**

- Task 4.

## Task 6: Write the operator runbook for the pilot

**Owner**

Main executor

**Status**

done

**Outcome**

A later session or another executor can run the pilot without rediscovering environment variables, target-repo setup, or how to inspect a failed run.

**Files**

- Modify: `README.md`
- Modify: `docs/guide/index.md`
- Create: `docs/guide/pilot.md`

**Changes**

1. Document the required environment variables and local filesystem layout for the service config, workspace root, thin SQLite operational state, and target-repository checkout/cache.
2. Document how to run `maestro run --once`, how to inspect structured logs and the minimal local state, how to inspect Linear writebacks, and how to replay the protocol probe when `app-server` behavior changes.
3. Keep the runbook pilot-scoped; do not document remote-worker or production rollout paths yet.
4. Verification completed with `rg -n "pilot|run --once|protocol probe" README.md docs/guide/index.md docs/guide/pilot.md`, `cargo run -- --help`, `cargo make fmt-check`, and `git diff --check`.

**Verification**

- `rg -n "pilot|run --once|protocol probe" README.md docs/guide/index.md docs/guide/pilot.md`
- `cargo run -- --help`
- `cargo make fmt-check`
- `git diff --check`

**Dependencies**

- Task 5.

## Rollout Notes

- Start with one bounded pilot deployment only.
- Keep the first execution lane single-issue and one-shot; add a forever poll loop only after `run --once` is reliable.
- Treat Linear as the operator-facing truth for issue progress and final result, with local state limited to operational needs.

## Suggested Execution

- Sequential: Tasks 1 through 6 should run in order because each step fixes a missing contract or runtime seam that the next task depends on.
- Parallelizable: After Task 2, documentation polish inside `README.md` and `docs/guide/` can happen in parallel with implementation only if it strictly follows the approved contracts from Task 2.
