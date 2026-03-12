# Workflow Contract Specification

Purpose: Define the machine-readable contract for downstream repository `WORKFLOW.md` files consumed by `maestro`.

Audience: Repository owners and `maestro` implementers.

## File location

- Each downstream target repository must place `WORKFLOW.md` at the repository root.
- `maestro` may also target itself. In that mode, this repository's own root `WORKFLOW.md` follows the same contract as any other downstream target repo.

## Parse model

- `WORKFLOW.md` consists of TOML frontmatter followed by Markdown body text.
- The TOML frontmatter delimiter is `+++`.
- The Markdown body is human-readable policy text that `maestro` may append to developer instructions.
- The frontmatter is the only machine-readable section of the file.

## Upstream divergences

- Upstream Symphony examples use YAML frontmatter. `maestro` intentionally uses TOML frontmatter instead.
- This divergence is deliberate and stable. Do not translate back to YAML only for stylistic upstream parity.

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
  - note: canonical stable project identity; this should match the Linear project `slugId`
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
- `validation_commands`
  - type: array of string
  - optional
  - default: `[]`

`validation_commands` run after agent execution and before the success writeback is committed.

## `[context]`

Purpose: Define additional repo-local context files that `maestro` should load early.

Supported keys:

- `read_first`
  - type: array of string
  - optional
  - default: `["AGENTS.md"]`

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
validation_commands = [
  "cargo make fmt-check",
  "cargo make lint",
  "cargo make test",
]

[context]
read_first = ["AGENTS.md", "docs/index.md"]
+++

Read `AGENTS.md` first.
Use `cargo make` whenever an equivalent task exists.
Use the issue-scoped tracker tools autonomously when tracker updates are required.
```

## Body semantics

- The Markdown body is repository policy text.
- `maestro` may append the body to developer instructions sent through `app-server`.
- The body should contain durable repo rules, not ephemeral run notes.
- The body should instruct the coding agent to use the issue-scoped tracker tools autonomously when tracker writes are part of the repo workflow.
