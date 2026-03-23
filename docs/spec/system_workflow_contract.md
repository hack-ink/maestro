# Workflow Contract Specification

Purpose: Define the machine-readable contract for downstream repository `WORKFLOW.md` files consumed by `maestro`.
Status: normative
Read this when: You are authoring, parsing, or validating a downstream repository `WORKFLOW.md` file for use by `maestro`.
Not this document: The `maestro` runtime state machine, the `app-server` protocol contract, or the operator pilot sequence.
Defines: The file location, parse model, supported frontmatter structure, and the required and optional `WORKFLOW.md` fields that `maestro` consumes.

## File location

- Each downstream target repository must place `WORKFLOW.md` at the repository root.
- `maestro` may also target itself. In that mode, this repository's own root `WORKFLOW.md` follows the same contract as any other downstream target repo.

## Parse model

- `WORKFLOW.md` consists of TOML frontmatter followed by Markdown body text.
- The TOML frontmatter delimiter is `+++`.
- The Markdown body is the primary repository-owned policy and prompt body that `maestro` injects into developer instructions.
- The frontmatter is the only machine-readable section of the file.

## Daemon reload semantics

- `maestro` daemon mode may defensively reload the configured repository-owned `WORKFLOW.md` on future poll ticks instead of relying on filesystem watchers.
- When a reload of the currently configured `WORKFLOW.md` path succeeds, future dispatch, retry, reconciliation after child exit, and prompt generation may use the new document immediately.
- When that same configured path fails to parse after at least one successful load, daemon mode must keep the last known good `WORKFLOW.md` active for future daemon decisions and log a warning instead of dropping the whole tick.
- An already running child lane keeps the workflow snapshot it started with; reload semantics affect later decisions, not mid-run prompt or reconciliation behavior for that active child.

## Upstream divergences

- Upstream Symphony examples use YAML frontmatter. `maestro` intentionally uses TOML frontmatter instead.
- This divergence is deliberate and stable. Do not translate back to YAML only for stylistic upstream parity.
- Upstream Symphony treats the `WORKFLOW.md` body as the primary repo-owned prompt and policy surface. `maestro` follows that model.
- `maestro` also supports `[context].read_first` as an optional local extension for extra repo-local context files. This extra-context surface is not part of the upstream Symphony spec and must not replace the primary `WORKFLOW.md` body.

## Required top-level fields

- `version`

Current supported value:

- `1`

## Optional tables

- `[tracker]`
- `[agent]`
- `[execution]`
- `[context]`

## `[tracker]`

Purpose: Define tracker-facing policy and workflow defaults.

Supported keys:

- `provider`
  - type: string
  - required
  - supported value for MVP: `"linear"`
- `project_slug`
  - type: string
  - required
  - note: canonical stable project identity; this should normally match the Linear project `slugId`
- `startable_states`
  - type: array of string
  - optional
  - default: `["Todo"]`
- `terminal_states`
  - type: array of string
  - optional
  - default: `["Done", "Canceled", "Duplicate"]`
- `in_progress_state`
  - type: string
  - optional
  - default: `"In Progress"`
- `success_state`
  - type: string
  - optional
  - default: `"In Review"`
  - note: `maestro` treats this as a PR-backed review handoff state, not a terminal completion state
- `completed_state`
  - type: string
  - optional
  - default: if omitted and `terminal_states` contains exact `"Done"`, the resolved completed state is `"Done"`; otherwise this field must be set explicitly
  - note: successful post-merge closeout target; when present, it must be a member of `terminal_states`
  - note: parser/load paths may remain permissive until post-merge closeout is implemented, but the runtime must stop for `manual_intervention_required` if closeout needs a completed state and workflow policy cannot resolve one
- `failure_state`
  - type: string
  - optional
  - default: `"Todo"`
- `opt_out_label`
  - type: string
  - optional
  - default: `"maestro:manual-only"`
- `needs_attention_label`
  - type: string
  - optional
  - default: `"maestro:needs-attention"`

## `[agent]`

Purpose: Define repo-local defaults for the direct `app-server` session.

Supported keys:

- `transport`
  - type: string
  - optional
  - default: `"stdio://"`
- `sandbox`
  - type: string
  - optional
  - default: `"workspace-write"`
- `approval_policy`
  - type: string
  - optional
  - default: `"never"`
- `model`
  - type: string
  - optional
- `personality`
  - type: string
  - optional
  - supported MVP values: `"none"`, `"friendly"`, `"pragmatic"`
- `service_tier`
  - type: string
  - optional

## `[execution]`

Purpose: Define repo-local execution and validation policy.

Supported keys:

- `max_attempts`
  - type: integer
  - optional
  - default: `3`
- `max_turns`
  - type: integer
  - optional
  - default: `1`
  - note: caps same-thread continuation turns inside one bounded run attempt; when omitted or set to `1`, Maestro preserves the current single-turn behavior
- `max_retry_backoff_ms`
  - type: integer
  - optional
  - default: `300000`
  - note: caps daemon-owned failure retry backoff in milliseconds; clean continuation retries use a separate short fixed delay in runtime policy
- `max_concurrent_agents`
  - type: integer
  - optional
  - default: `1`
  - note: upper-bounds concurrent `maestro` runs per repository; values must be greater than or equal to `1`
- `max_concurrent_agents_by_state`
  - type: table of `state_name = integer`
  - optional
  - default: `{}` (no per-state overrides)
  - note: further limits concurrency for specific tracker states; each configured state must use a positive value that does not exceed `max_concurrent_agents`
- `validation_commands`
  - type: array of string
  - optional
  - default: `[]`

`validation_commands` run after agent execution and before the success writeback is committed.

## `[context]`

Purpose: Define optional additional repo-local context files that `maestro` should load alongside the primary `WORKFLOW.md` body.

Supported keys:

- `read_first`
  - type: array of string
  - optional
  - default: `[]`

Paths are repository-relative.

## Forbidden content in frontmatter

The frontmatter must not include:

- machine-local absolute paths
- credentials or secrets
- host-specific workspace roots
- per-operator personal preferences that are not repository policy

Those values belong in `maestro` service configuration, not in `WORKFLOW.md`.

## Example

```md
+++
version = 1

[tracker]
provider = "linear"
project_slug = "pubfi"
startable_states = ["Todo"]
terminal_states = ["Done", "Canceled", "Duplicate"]
in_progress_state = "In Progress"
success_state = "In Review"
failure_state = "Todo"
opt_out_label = "maestro:manual-only"
needs_attention_label = "maestro:needs-attention"

[agent]
transport = "stdio://"
sandbox = "workspace-write"
approval_policy = "never"
personality = "pragmatic"

[execution]
max_attempts = 3
max_retry_backoff_ms = 300000
max_concurrent_agents = 2
max_concurrent_agents_by_state = { "In Progress" = 2, "Todo" = 1 }
validation_commands = [
  "cargo make fmt-check",
  "cargo make lint",
  "cargo make test",
]
+++
Use `cargo make` whenever an equivalent task exists.
Use the issue-scoped tracker tools autonomously when tracker updates are required.
```

## Body semantics

- The Markdown body is repository policy text.
- Issue-scoped developer instructions should include the `WORKFLOW.md` body first, then any optional `context.read_first` files, then the explicit tracker tool contract.
- The body should contain durable repo rules, not ephemeral run notes.
- The body should instruct the coding agent to use the issue-scoped tracker tools autonomously when tracker writes are part of the repo workflow.
- `context.read_first` defaults to empty and should be reserved for optional extra repo-local files beyond the primary `WORKFLOW.md` body.
- If the repository expects PR-backed review handoff, the body should state that the lane must produce a reviewable PR before the success state can be reached.
