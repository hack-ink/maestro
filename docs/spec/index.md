# Spec Index

Purpose: Provide the canonical entry point for repository specifications.

Audience: This documentation is written for LLM consumption and should remain explicit and unambiguous.

## Structure

- Store specs directly under `docs/spec/` (flat structure).
- Use descriptive file names with stable prefixes (`system_`, `t0_`, `t1_`, `trace_`, `search_`).
- Link new specs from `docs/index.md` or `docs/guide/index.md` when relevant.

## Current system specs

- `docs/spec/system_maestro_runtime.md` for the orchestration state machine, Linear writeback rules, and local operational-state boundaries.
- `docs/spec/system_workflow_contract.md` for the downstream `WORKFLOW.md` machine-readable contract.
- `docs/spec/system_app_server_contract.md` for the direct `codex app-server` protocol boundary used by the MVP.
- `docs/spec/system_tracker_tool_contract.md` for the issue-scoped tracker tool boundary used by agent-owned tracker writes.

## Authoring guidance (LLM-first)

- Use explicit nouns instead of pronouns whenever possible.
- Define acronyms and domain terms on first use.
- Prefer short sentences with one idea each.
- Include canonical field names, enums, units, and constraints.
- Provide small, concrete examples for non-obvious flows.
- Keep links stable and prefer absolute repo paths.
