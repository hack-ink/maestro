# App-Server Contract Specification

Purpose: Define the direct `codex app-server` protocol boundary used by the `maestro` MVP.
Status: normative
Read this when: You are implementing or validating `maestro`'s direct `codex app-server` integration, including transport, handshake, request flow, or dynamic tools.
Not this document: The runtime state machine, downstream `WORKFLOW.md` policy, or operator runbooks.
Defines: The supported transport, protocol source-of-truth boundary, required request and notification flow, and the MVP contract for `initialize`, `thread/start`, and repeated `turn/start` calls on one thread.

## Transport

- The MVP transport is `stdio://`.
- `maestro` starts the child process with:

```sh
codex app-server --listen stdio://
```

- `ws://` is out of scope for the MVP.

## Source of truth

- The generated JSON Schema bundle is the authoritative local protocol source.
- Generate the bundle with:

```sh
codex app-server generate-json-schema --experimental --out /tmp/maestro-app-server-schema-check
```

- `maestro` must treat the generated schema as more authoritative than stale handwritten assumptions.
- `--experimental` is required when inspecting `dynamicTools` and related experimental fields in the generated bundle.

## Implementation guidance

- When implementing Maestro features that depend on Codex runtime behavior, read the relevant Codex or `app-server` implementation path, not only this contract.
- This is especially required for features such as idle timeout policy, stall detection, retry boundaries, waiting-state handling, and any other liveness-sensitive behavior.
- Use this document to constrain protocol shape, then use the upstream implementation to adapt Maestro behavior to how Codex actually emits progress, waits, and terminates turns.
- Do not finalize these features from local heuristics alone when the upstream runtime behavior can be inspected directly.

## Upstream alignment

- Upstream Symphony remains the ownership reference for the orchestration boundary.
- `maestro` keeps one deliberate contract divergence here: TOML frontmatter in downstream `WORKFLOW.md`.
- For the next phase, the preferred tracker-tool transport is a client-side dynamic tool bridge handled inside the existing JSON-RPC client.
- Rationale: the local generated schema already exposes server-driven dynamic tool call requests (`item/tool/call`) and related tool-call notifications, so `maestro` can service issue-scoped tracker writes without introducing a second child service for the first dogfood pilot.
- A process-local MCP server remains a future option if the tool surface expands or if the dynamic bridge proves too constrained.

## Protocol shape

- Protocol family: JSON-RPC request/response plus asynchronous notifications.
- Required client requests for the MVP:
  - `initialize`
  - `thread/start`
  - `turn/start`
- Required notifications for the MVP:
  - `thread/started`
  - `thread/status/changed`
  - `turn/started`
  - `turn/completed`

Additional notifications may be recorded opportunistically for diagnostics.

The follow-up alignment phase should also record tool-related requests and notifications needed for issue-scoped tracker writes.

## Required request flow

1. Start the child process.
2. Send `initialize`.
3. Send `thread/start`.
4. Send `turn/start`.
5. Consume notifications until that turn reaches a terminal outcome.
6. If the repo-owned continuation policy allows another same-thread turn, send another `turn/start` on the same thread.
7. Persist the local run journal and classify the bounded run result.

When dynamic tools are enabled, `maestro` must also:

1. Register the tool surface in `thread/start.dynamicTools`.
2. Answer `item/tool/call` requests with `DynamicToolCallResponse`.
3. Serialize dynamic tool output items with schema-approved `type` values such as `inputText`.
4. Keep every `dynamicTools[].name` within the app-server identifier pattern `^[a-zA-Z0-9_-]+$`.

## `initialize`

Method:

- `initialize`

Required params:

- `clientInfo.name`
- `clientInfo.version`

Optional params:

- `capabilities.experimentalApi`
- `capabilities.optOutNotificationMethods`

`maestro` should declare itself explicitly as a non-interactive orchestration client.
- `dynamicTools` requires `capabilities.experimentalApi = true` during `initialize`.
- This experimental API enablement is part of the JSON-RPC handshake, not a `features.*` config flag in `~/.codex/config.toml`.

## `thread/start`

Method:

- `thread/start`

The MVP thread start request owns these fields:

- `cwd`
- `dynamicTools` when the run exposes issue-scoped tracker tools
- `developerInstructions`
- `personality` when configured
- `serviceTier` when configured

Maestro must not inject repo-owned sandbox or approval-policy overrides into `thread/start`. Child runs inherit execution policy from the active Codex runtime.

`ThreadStartResponse` returns the effective thread plus the effective execution settings.

## `turn/start`

Method:

- `turn/start`

Required params:

- `threadId`
- `input`

The MVP turn start request owns these fields:

- `threadId`
- `input`
- optional overrides for `cwd`, `personality`, and `serviceTier` when the run needs turn-level overrides

`TurnStartResponse` returns the accepted turn object.

Within one bounded Maestro run attempt, the runtime may start multiple turns on the same thread. Thread-level settings remain stable from `thread/start`; continuation policy such as `execution.max_turns` and between-turn tracker revalidation stays in Maestro, not in the app-server protocol.

## Notification handling

### `thread/started`

- Record the created thread identifier.

### `thread/status/changed`

- Track whether the thread is `active`, `idle`, `systemError`, or `notLoaded`.
- `waitingOnApproval` and `waitingOnUserInput` are policy violations for the MVP because Maestro runs are non-interactive and must inherit a Codex runtime policy that does not require manual approval or user input.

### `turn/started`

- Record the turn identifier and transition the local run into `running`.

### `turn/completed`

- Record the completed turn payload.
- Classify the turn as success, retryable failure, or terminal failure.

## Error handling

- JSON-RPC transport failure before `thread/start` succeeds is a retryable startup failure.
- `thread/status/changed` with `systemError` is a failed run.
- Turn completion with codex error information must be classified into:
  - retryable failure
  - terminal failure requiring human attention

The failure classifier must consider retry budget from the repo `WORKFLOW.md` policy.

## Ownership boundaries

`maestro` owns:

- child process lifecycle
- request identifiers
- JSON-RPC framing
- local journaling of protocol messages
- mapping repo workflow policy into request fields other than inherited runtime execution policy
- servicing issue-scoped tracker tool calls
- run classification and fallback reconciliation writes when the agent never reached a tracker update

The downstream repository policy owns:

- repo-specific instructions
- validation commands
- issue eligibility parameters
- workflow state and label names
- when and why the coding agent should perform tracker writes during the run

## Probe contract

The `protocol probe` command must verify at least:

1. `codex app-server` is invocable locally.
2. Schema generation succeeds.
3. The schema contains `initialize`, `thread/start`, and `turn/start`.
4. The schema contains `thread/status/changed` and `turn/completed`.
5. The local client can complete one `dynamicTools -> item/tool/call -> response` round trip and still finish with the expected final output.

The probe command is the first gate before deeper orchestrator logic depends on the protocol.
