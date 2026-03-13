# Tracker Tool Contract Specification

Purpose: Define the issue-scoped tracker tool surface that allows the coding agent to update the currently leased issue autonomously while keeping `maestro` in control of orchestration lifecycle and safety.
Status: normative
Read this when: You are implementing, reviewing, or constraining the issue-scoped tracker tool bridge used during a `maestro` run.
Not this document: The full `maestro` runtime state machine, the downstream `WORKFLOW.md` contract, or the end-to-end pilot runbook.
Defines: The tracker ownership boundary, preferred transport, issue-scoped tool surface, policy constraints, and failure-handling rules for tracker writes.

## Ownership boundary

- The coding agent should normally perform tracker writes for the currently leased issue during the run.
- `maestro` still owns:
  - lease acquisition and release
  - workspace lifecycle
  - retries and retry budget enforcement
  - startup reconciliation
  - crash recovery and terminal fallback writes
- The coding agent must not gain broad tracker write access beyond the currently leased issue.

## Preferred transport

- Preferred follow-up transport: a client-side dynamic tool bridge handled inside the existing `app_server` JSON-RPC client.
- Evidence source: the local `codex app-server generate-json-schema` bundle exposes server-driven dynamic tool call requests (`item/tool/call`) and related tool-call notifications.
- Deferred alternative: a process-local MCP server may be introduced later if the required tool surface grows beyond what the dynamic bridge can represent safely.

## Scope model

- Every run attempt leases exactly one tracker issue.
- The tool bridge must bind the agent to that single leased issue identifier.
- Tool calls that reference any other issue must be rejected.
- Tool calls that request unsupported operations must be rejected.

## Minimum tool surface

The follow-up MVP should support these issue-scoped operations:

- `issue_transition`
  - move the current issue to an allowed target state
- `issue_comment`
  - add a comment to the current issue
- `issue_label_add`
  - add a label to the current issue when workflow policy requires it

Additional operations such as PR-link attachment or richer metadata updates may be added later, but they are not required for the first self-dogfood pilot.

## Policy constraints

- Allowed target states should be constrained by repo workflow policy plus the orchestration phase.
- The tool bridge should reject transitions that violate the current repo workflow contract.
- Comment bodies should remain repository-controlled or agent-authored, but all tool calls must be journaled by `maestro` for recovery and audit.
- Structured comment fields such as `worktree_path` must use repository-relative paths; absolute host paths should be rejected before writing to the tracker.
- Dynamic tool names must satisfy the `codex app-server` identifier restriction `^[a-zA-Z0-9_-]+$`; dotted names are invalid.

## Failure handling

- If the agent never reaches a tracker write, `maestro` may perform a minimal fallback write during reconciliation or terminal failure handling.
- If a tracker tool call fails transiently, the failure should be surfaced to the run journal so retry logic can reason about it.
- If a tracker tool call fails because it targeted the wrong issue or an unsupported operation, treat that as a policy violation, not as a retryable transport error.

## Future expansion

- A later phase may lift the transport from a dynamic tool bridge to a process-local MCP server if broader tracker or repo-collaboration tools are required.
- Any future expansion must preserve the issue-scoped safety boundary unless the user explicitly approves a broader trust model.
