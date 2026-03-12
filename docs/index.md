# Documentation Index

Purpose: Provide the canonical entry point and reading order for repository documentation.

## Start here

- `AGENTS.md` for automated agent rules and tooling constraints.
- `docs/spec/index.md` for normative system specifications and contracts.
- `docs/guide/index.md` for operational guides and runbooks.
- `docs/governance.md` for documentation structure and update rules.
- `docs/plans/` for Claude-generated execution plans (non-normative).

## Documentation classes

### Specifications (normative)

- Location: `docs/spec/` (flat structure).
- Use for: System contracts, data models, pipeline behavior, and required invariants.
- Entry point: `docs/spec/index.md`.
- Current core specs:
  - `docs/spec/system_maestro_runtime.md`
  - `docs/spec/system_workflow_contract.md`
  - `docs/spec/system_app_server_contract.md`
  - `docs/spec/system_tracker_tool_contract.md`

### Operational and pipeline docs (implementation guides)

- Location: `docs/guide/`
- Use for: Runbooks, pipeline walkthroughs, operational maintenance, and test procedures.
- Entry point: `docs/guide/index.md`.

### Working plans and drafts

- Location: `docs/plans/`
- Use for: Temporary design docs and execution plans that may drift.

### Repository README

- Location: `README.md` (the only README in the repository).
- Use for: High-level project overview and entry points into `docs/`.
