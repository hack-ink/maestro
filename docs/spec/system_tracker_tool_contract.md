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
- `issue_review_handoff`
  - validate and record a PR-backed success handoff for the current issue
- `issue_label_add`
  - add a label to the current issue when workflow policy requires it
- `issue_terminal_finalize`
  - explicitly finalize the current run's terminal tracker path after the required tracker writes already exist

Additional operations such as richer metadata updates may be added later, but they are not required for the first PR-backed self-dogfood pilot.

## Completion signal contract

At turn completion, the issue-scoped tool bridge must leave `maestro` with exactly one terminal completion signal for the leased issue and a matching explicit terminal-finalization call:

- `review_handoff`
  - produced by `issue_review_handoff`
  - finalized by `issue_terminal_finalize(path = "review_handoff")`
  - means the lane is claiming review-ready success
- `manual_attention`
  - produced by adding the configured `needs_attention_label` and leaving an explanatory comment
  - finalized by `issue_terminal_finalize(path = "manual_attention")`
  - means the lane is explicitly handing the issue back to a human instead of asking for `In Review`

Invalid outcomes:

- both signals are present
- neither signal is present
- a signal is present, but the matching `issue_terminal_finalize` call never happened
- `issue_terminal_finalize` names a different path than the currently recorded terminal signal

In either invalid case, `maestro` must fail the attempt rather than infer which path the agent intended.

## Policy constraints

- Allowed target states should be constrained by repo workflow policy plus the orchestration phase.
- The tool bridge should reject transitions that violate the current repo workflow contract.
- Generic `issue_transition` must not move the current issue directly into the configured success state.
- `issue_review_handoff` must validate that the supplied PR belongs to the current repository and lane branch, points at the validated lane HEAD, is open, and is ready for review before `maestro` accepts the handoff.
- `issue_review_handoff` records the success metadata during the turn, but `maestro` owns the final completion comment and `In Review` transition after service-side validation succeeds.
- Adding the configured `needs_attention_label` is an explicit human-required failure exit for the active lane. In that case the agent must leave a comment explaining the blocker, must not also record `issue_review_handoff`, and `maestro` must stop automatic retries for that attempt.
- Human-attention comments must describe the exact observed blocker and should include the failed command plus raw error text when available. The agent must not speculate about capabilities or environment restrictions that it did not directly verify.
- The human-attention exit is not complete until the explanatory comment is successfully written after the label request. A label-only signal must be rejected as an invalid completion disposition.
- The run is not complete until `issue_terminal_finalize` succeeds against the matching terminal path. A saved plan reaching `phase = "done"` or an agent summary message is not a substitute.
- Issues that carry the configured `needs_attention_label` must remain ineligible for future automatic selection until a human clears the label.
- `issue_review_handoff` and the human-attention exit are mutually exclusive terminal signals for the same turn.
- Before a live run starts, `maestro` must preflight the local GitHub CLI dependency used for review handoff inspection instead of discovering a missing `gh` binary only after an otherwise successful turn.
- Comment bodies should remain repository-controlled or agent-authored, but all tool calls must be journaled by `maestro` for recovery and audit.
- Structured comment fields such as `workspace_path` must use repository-relative paths; absolute host paths should be rejected before writing to the tracker.
- Dynamic tool names must satisfy the `codex app-server` identifier restriction `^[a-zA-Z0-9_-]+$`; dotted names are invalid.

## Failure handling

- If the agent never reaches a tracker write, `maestro` may perform a minimal fallback write during reconciliation or terminal failure handling.
- If a tracker tool call fails transiently, the failure should be surfaced to the run journal so retry logic can reason about it.
- If a tracker tool call fails because it targeted the wrong issue or an unsupported operation, treat that as a policy violation, not as a retryable transport error.
- If the turn completes without a valid recorded `issue_review_handoff` and without an explicit human-attention exit, `maestro` must treat the run as failed rather than silently moving the issue to `In Review`.
- If the turn completes without a matching `issue_terminal_finalize` call for the resolved terminal path, `maestro` must treat the run as failed before reporting the attempt as successful.
- If PR-backed success writeback partially succeeds, for example the issue reaches `In Review` but the completion comment fails to post, `maestro` must treat the lane as human-required and must not place it back on the automatic retry path.

## Future expansion

- A later phase may lift the transport from a dynamic tool bridge to a process-local MCP server if broader tracker or repo-collaboration tools are required.
- Any future expansion must preserve the issue-scoped safety boundary unless the user explicitly approves a broader trust model.
