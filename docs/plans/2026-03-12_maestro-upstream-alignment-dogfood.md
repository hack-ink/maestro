# Maestro Upstream Alignment and Self-Dogfood Plan

## Goal

Realign `maestro` with the important ownership and runtime boundaries from upstream Symphony, keep TOML frontmatter as an intentional helixbox divergence, and make `maestro` itself the first downstream target repository so the next execution loop can be dogfooded locally before touching other repos.

## Scope

- Keep `WORKFLOW.md` as Markdown plus TOML frontmatter and document that choice as a deliberate divergence from upstream.
- Move tracker write ownership from service-owned Linear mutations toward agent-owned issue writes through a narrow issue-scoped tool contract.
- Harden the Linear adapter to use a stable project identifier, pagination, and reconciliation-oriented reads instead of project-name equality only.
- Add the missing orchestration behaviors that matter for self-dogfooding: startup reconciliation, ineligible-stop handling, and terminal workspace cleanup.
- Make this repository a first-class target repo with its own root `WORKFLOW.md`, safe pilot config guidance, and one bounded pilot flow.

## Non-goals

- Replacing TOML frontmatter with YAML.
- Switching from Rust to Elixir or otherwise cloning the upstream implementation structure.
- Generalizing to multi-node workers, remote control planes, or production rollout in this phase.
- Pointing the first live pilot at `pubfi-mono` or another production-adjacent repo before `maestro` dogsfoods on itself.
- Granting the coding agent broad Linear workspace write access outside the currently leased issue.

## Constraints

- The completed MVP plan at [2026-03-12_maestro-app-server-mvp.md](./2026-03-12_maestro-app-server-mvp.md) is the baseline. This follow-up plan should extend, not relitigate, the finished scaffolding.
- Repo-native verification must continue to run through `cargo make` from the repository root because [Makefile.toml](../../Makefile.toml) remains the source of truth for checks.
- The direct `app-server` path stays in scope. Do not pivot to the SDK bridge while doing this alignment pass.
- The live pilot should keep blast radius low. `dry-run` remains the first gate, and the first live run should target `maestro` itself rather than a different downstream repo.
- The service must continue to own leases, workspace lifecycle, retries, and crash recovery even after tracker writes move toward the coding agent.

## Open Questions

- None.

## Execution State

- Last Updated: 2026-03-12
- Next Checkpoint: Task 6
- Blockers: none in architecture. The next dry run depends only on a local `maestro.toml` whose `tracker.api_key` is set to either a literal Linear token or a `$ENV_VAR` reference.

## Decision Notes

- Keep TOML frontmatter. This is a deliberate helixbox divergence and does not need to converge back to YAML just to look more upstream-shaped.
- Treat upstream Symphony as the architectural reference for ownership boundaries: the service should schedule and reconcile, while the coding agent should normally perform issue-scoped tracker writes autonomously through tools.
- Preferred follow-up tracker tool transport: a client-side dynamic tool bridge handled inside the existing `app_server` JSON-RPC client, based on the local schema exposing server-driven tool call requests.
- Dynamic tool registration shape is now verified locally: `initialize.capabilities.experimentalApi = true` plus `thread/start.dynamicTools = [...]`. `--experimental` is required for schema generation, but runtime enablement lives in the handshake rather than a `features.*` config flag.
- Canonical Linear project identity for service config and workflow policy: `project_slug`, matching the upstream Symphony notion and mapping to Linear project `slugId` in the reader query layer.
- Preserve the current workspace-first lane model. One Linear issue should still map to one isolated branch and one `git workspace` checkout.
- Use `maestro` itself as the first target repo because it gives the cleanest dry-run and first-live-run path with the lowest external blast radius.
- Local reconciliation state now scopes leases and workspace mappings by configured `id` so one `maestro.toml` manages one project lane without cross-project cleanup.
- The first self-dogfood tracker scope is the bounded helixbox Linear project `Maestro MVP`, currently identified by project slug `maestro-mvp-10bbdae9b904`.

## Implementation Outline

The follow-up should start by separating "what we are intentionally different on" from "what is merely incomplete or wrong." TOML frontmatter stays, but tracker write ownership, project-identifier handling, and reconciliation semantics should move closer to upstream Symphony. The right shape is a service that still owns lifecycle and safety while handing the coding agent a narrow, issue-scoped tracker tool surface so status transitions and comments happen autonomously during the run rather than through hardcoded service-side writebacks.

The technical hinge is the tracker tool bridge. The local `app-server` schema already shows both MCP-related request/notification shapes and server-driven dynamic tool calls, so agent-owned tracker writes are protocol-feasible. The preferred follow-up transport is the smaller dynamic tool bridge that the existing `app_server` client answers directly. That keeps the first dogfood pilot issue-scoped without introducing a second child service before the tracker surface has proven itself.

Once the tracker boundary is corrected, the rest of the work should make `maestro` self-hostable as a target repo: add a root `WORKFLOW.md`, migrate config away from project-name equality, teach the orchestrator to reconcile and clean up in more upstream-like ways, and then use a bounded `maestro` pilot project or equivalent scope to run `dry-run` first and one carefully chosen live issue second.

## Task 1: Lock the upstream-alignment contract

**Owner**

Main executor

**Status**

done

**Outcome**

The repository has an explicit written contract for deliberate divergences versus required alignments, and the tracker-write ownership model is defined precisely enough that implementation can proceed without guessing.

**Files**

- Modify: `docs/spec/index.md`
- Modify: `docs/index.md`
- Modify: `docs/spec/system_maestro_runtime.md`
- Modify: `docs/spec/system_workflow_contract.md`
- Modify: `docs/spec/system_app_server_contract.md`
- Modify: `docs/guide/pilot.md`
- Create: `docs/spec/system_tracker_tool_contract.md`

**Changes**

1. Add an explicit "Upstream Divergences" section that records TOML frontmatter as intentional and records service-owned tracker writes as no longer the target model.
2. Define the new tracker ownership boundary: the coding agent writes state, comments, and handoff data autonomously through issue-scoped tools, while `maestro` retains lifecycle, retries, leases, and fallback reconciliation authority.
3. Choose and document the preferred issue-scoped tracker tool transport for the next implementation checkpoint.
4. Tighten the pilot guide so it no longer describes service-owned start/success comments as the intended steady state once this phase lands.
5. Verification completed with `rg -n "Upstream divergences|tracker ownership|issue-scoped" docs/spec docs/guide/pilot.md`, `cargo make fmt-check`, and `git diff --check`.

**Verification**

- `rg -n "Upstream divergences|tracker ownership|issue-scoped" docs/spec docs/guide/pilot.md`
- `cargo make fmt-check`
- `git diff --check`

**Dependencies**

- None.

## Task 2: Harden service config and the Linear reader boundary

**Owner**

Main executor

**Status**

done

**Outcome**

`maestro` no longer depends on project-name equality for issue discovery and has the reader-side tracker primitives needed for pagination, issue refresh, and reconciliation.

**Files**

- Modify: `src/config.rs`
- Modify: `src/workflow.rs`
- Modify: `src/orchestrator.rs`
- Modify: `src/tracker/mod.rs`
- Modify: `src/tracker/linear.rs`
- Modify: `docs/spec/system_workflow_contract.md`
- Modify: `docs/guide/pilot.md`

**Changes**

1. Replace project-name equality with canonical `tracker.project_slug`, and mirror the same stable identifier in downstream `WORKFLOW.md`.
2. Add paginated issue listing instead of a single `first: 50` project query.
3. Add tracker reads needed for reconciliation, including refreshing issue state for known issue IDs and reading project metadata from the chosen stable identifier.
4. Update the orchestrator contract validation so service config and `WORKFLOW.md` align on `project_slug` rather than project-name equality.
5. Update the workflow contract doc and pilot guide so operators use the canonical project identifier instead of the old project-name shortcut.
6. Verification completed with `cargo make fmt-check`, `cargo make lint`, `cargo make test`, and `git diff --check`.

**Verification**

- `cargo make fmt-check`
- `cargo make lint`
- `cargo make test`

**Dependencies**

- Task 1.

## Task 3: Move tracker writes onto an issue-scoped agent tool bridge

**Owner**

Main executor

**Status**

done

**Outcome**

The orchestrator no longer performs the normal-path Linear state transitions and comments itself. Instead, the coding agent can autonomously mutate only the currently leased issue through a narrow tool surface, while the service retains safety fallbacks.

**Files**

- Modify: `src/agent/app_server.rs`
- Modify: `src/agent/json_rpc.rs`
- Modify: `src/agent/mod.rs`
- Modify: `src/orchestrator.rs`
- Modify: `src/tracker/mod.rs`
- Modify: `src/tracker/linear.rs`
- Create: `src/agent/tracker_tool_bridge.rs`

**Changes**

1. Implement the issue-scoped tracker tool bridge chosen in Task 1 so the app-server session can service agent-driven tracker writes safely.
2. Remove service-owned start/success writebacks from the happy path and update prompts or workflow context so the agent is responsible for autonomous tracker updates during the run.
3. Keep explicit service-owned fallback behavior only for crash recovery, reconciliation, and terminal failure cases where the agent never reached the point of writing the tracker.
4. Add tests for issue scoping, allowed operations, and denial of writes outside the currently leased issue.
5. Completed local protocol alignment: `maestro` now sends `initialize.capabilities.experimentalApi = true`, registers `thread/start.dynamicTools`, verifies the round trip with `cargo run -- protocol probe`, and removes service-owned start/success writebacks from the happy path while preserving service-owned failure fallback.

**Verification**

- `cargo run -- protocol probe`
- `cargo make fmt-check`
- `cargo make lint`
- `cargo make test`

**Dependencies**

- Task 2.

## Task 4: Add reconciliation, ineligible-stop, and cleanup semantics

**Owner**

Main executor

**Status**

done

**Outcome**

`maestro` behaves more like an upstream scheduler/runner: it can reconcile startup state, stop or skip runs whose issues became ineligible, and clean up workspaces once issues become terminal.

**Files**

- Modify: `src/orchestrator.rs`
- Modify: `src/state.rs`
- Modify: `src/workspace.rs`
- Modify: `docs/spec/system_maestro_runtime.md`
- Modify: `docs/guide/pilot.md`

**Changes**

1. Add startup reconciliation that compares local leases and retained run state against current tracker state before starting new work.
2. Implement the minimum stop/skip semantics needed when an issue becomes terminal or otherwise ineligible while `maestro` is preparing or reconsidering a lane.
3. Add terminal workspace cleanup and retention rules that match the runtime spec instead of leaving cleanup as a future-only note.
4. Document the operator-visible behavior for these transitions in the runtime spec and pilot guide.
5. Scope local issue leases and workspace mappings by configured `id` so reconciliation and cleanup only operate within the current project lane.

**Verification**

- `cargo make fmt-check`
- `cargo make lint`
- `cargo make test`
- `cargo run -- run --once --dry-run --config ./maestro.toml`

**Dependencies**

- Task 3.

## Task 5: Make `maestro` a first-class target repo

**Owner**

Main executor

**Status**

done

**Outcome**

This repository can be targeted by `maestro` itself with an explicit root `WORKFLOW.md`, documented pilot config, and a bounded issue scope chosen for self-dogfooding.

**Files**

- Create: `WORKFLOW.md`
- Modify: `README.md`
- Modify: `docs/guide/pilot.md`
- Modify: `docs/spec/system_workflow_contract.md`

**Changes**

1. Add a root `WORKFLOW.md` for this repository using TOML frontmatter plus repo-local policy body.
2. Document the recommended self-dogfood scope and safety posture so the first pilot uses a bounded `maestro` issue stream rather than an open-ended backlog.
3. Update the pilot guide so the default narrative is "run `maestro` against `maestro` first," including dry-run-first and workspace-root guidance.
4. Record the current self-dogfood project identity from helixbox Linear so the root `WORKFLOW.md` and pilot guide use the real `Maestro MVP` slug rather than a placeholder.

**Verification**

- `rg -n "maestro" WORKFLOW.md README.md docs/guide/pilot.md`
- `cargo make fmt-check`
- `cargo run -- run --once --dry-run`

**Dependencies**

- Task 4.

## Task 6: Run the first self-dogfood pilot

**Owner**

Main executor

**Status**

pending

**Outcome**

One carefully chosen `maestro` issue has been exercised through the new ownership model, first in `dry-run`, then in one live run, with enough evidence captured to decide whether the next target repo can be onboarded safely.

**Files**

- Modify: `docs/plans/2026-03-12_maestro-upstream-alignment-dogfood.md`
- Modify: `docs/guide/pilot.md`

**Changes**

1. Run `protocol probe` and `run --once --dry-run` against the self-targeting config and record any contract drift discovered during the dry run.
2. Execute one live run on a low-risk `maestro` issue and capture the observed tracker behavior, workspace lifecycle, and failure-recovery notes.
3. Update the plan and pilot guide with the real pilot evidence and any follow-up blockers before onboarding another target repo.

**Verification**

- `cargo run -- protocol probe`
- `cargo run -- run --once --dry-run --config ./maestro.toml`
- `cargo run -- run --once --config ./maestro.toml`
- `cargo make fmt-check`
- `cargo make lint`
- `cargo make test`

**Dependencies**

- Task 5.

## Rollout Notes

- Prefer a bounded `maestro` pilot scope rather than an unbounded all-issues project. Keep the first dogfood lane intentionally small.
- Keep `dry-run` as the first operator gate even after the tracker ownership model changes.
- If the tracker tool bridge cannot be issue-scoped safely, stop and resolve that before the first live dogfood run instead of falling back to broad write permissions silently.

## Suggested Execution

- Sequential: Tasks 1 through 6 should run in order because the tracker ownership decision determines the implementation seam, which determines reconciliation behavior, which determines whether the self-dogfood pilot is safe.
- Parallelizable: After Task 1, documentation-only updates to the pilot guide and README can proceed in parallel with code work if they do not guess beyond the finalized tracker-tool contract.
