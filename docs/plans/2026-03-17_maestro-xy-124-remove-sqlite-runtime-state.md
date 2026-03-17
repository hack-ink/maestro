```json
{
  "spec": {
    "schema": "plan/1",
    "plan_id": "maestro-xy-124-remove-sqlite-runtime-state-2026-03-17",
    "goal": "Implement XY-124 by removing durable SQLite from Maestro runtime state while preserving daemon supervision through process-memory state and tracker/filesystem-driven recovery.",
    "success_criteria": [
      "Normal Maestro operation no longer requires a durable SQLite database file or rusqlite-backed state path.",
      "Daemon runtime state for leases, run attempts, event activity, and workspace mappings lives in process memory only.",
      "Restart recovery rebuilds retained workspace knowledge and active-issue redispatch intent from current Linear issue state plus local `.workspaces` inspection.",
      "Operator docs and runtime specs no longer tell users to inspect `maestro.sqlite3` as the authoritative recovery path.",
      "Repo-native tests cover the in-memory runtime store plus restart-recovery behavior for retained workspaces and active issues."
    ],
    "constraints": [
      "Do not introduce another local durable database or file-backed runtime state cache.",
      "Keep Linear as the durable workflow database of record.",
      "Keep TOML frontmatter as the only machine-readable WORKFLOW.md format.",
      "Do not widen scope into SSH workers or unrelated archived-plan cleanup.",
      "Preserve the current single-dispatch-slot model unless the runtime proves it must change."
    ],
    "defaults": {
      "authority_issue": "XY-124",
      "config_path": "./tmp/maestro.toml",
      "workspace_root": ".workspaces",
      "verification_commands": [
        "cargo make lint-fix",
        "cargo make fmt",
        "cargo make test",
        "cargo make lint",
        "cargo make fmt-rust-check",
        "cargo make fmt-toml-check",
        "git diff --check"
      ]
    },
    "tasks": [
      {
        "id": "codify-memory-runtime-contract",
        "title": "Codify the remove-SQLite runtime contract",
        "status": "done",
        "objective": "Make the no-durable-state boundary explicit in plans, specs, and operator docs before editing runtime code.",
        "inputs": [
          "XY-124 issue scope",
          "Current SQLite-backed state surfaces in src/state.rs, src/orchestrator.rs, and docs/guide/pilot.md",
          "Current structural follow-ups sequencing"
        ],
        "outputs": [
          "Updated umbrella and structural plans pointing to XY-124",
          "A dedicated XY-124 execution plan with explicit restart-recovery semantics"
        ],
        "verification": [
          "Plan authority no longer points at XY-128 as the active structural lane",
          "The plan explicitly states that tracker state plus filesystem inspection replace durable SQLite"
        ],
        "depends_on": []
      },
      {
        "id": "replace-sqlite-state-store",
        "title": "Replace the SQLite-backed store with process-memory runtime state",
        "status": "done",
        "objective": "Swap rusqlite-backed leases, run attempts, event journal, and workspace mappings for an in-memory runtime store used by orchestrator and app-server.",
        "inputs": [
          "Current StateStore API",
          "Current orchestrator and app-server call sites",
          "Runtime contract from codify-memory-runtime-contract"
        ],
        "outputs": [
          "An in-memory StateStore implementation",
          "No production path that opens or writes `maestro.sqlite3`"
        ],
        "verification": [
          "StateStore unit tests pass against the new implementation",
          "The runtime builds without depending on a durable SQLite path"
        ],
        "depends_on": [
          "codify-memory-runtime-contract"
        ]
      },
      {
        "id": "recover-from-tracker-and-workspaces",
        "title": "Recover runtime state from tracker and retained workspaces",
        "status": "done",
        "objective": "Teach daemon startup and status surfaces to rebuild the actionable runtime view from current Linear issue state plus local workspace inspection.",
        "inputs": [
          "In-memory runtime store",
          "Current retry/dispatch policy",
          "WorkspaceManager lane conventions"
        ],
        "outputs": [
          "Recovery logic for retained workspace mappings after process restart",
          "Recovery logic for active redispatch candidates without relying on local durable state"
        ],
        "verification": [
          "Tests cover retained-workspace recovery after empty-state startup",
          "Tests cover restart-time treatment of active `In Progress` issues and terminal workspace cleanup"
        ],
        "depends_on": [
          "replace-sqlite-state-store"
        ]
      },
      {
        "id": "align-operator-surface-and-docs",
        "title": "Align status output, runtime spec, and pilot docs with memory-only state",
        "status": "done",
        "objective": "Remove operator guidance that treats SQLite as the source of truth and explain the new restart-recovery model clearly.",
        "inputs": [
          "Recovered runtime/status behavior",
          "Current runtime spec and pilot guide"
        ],
        "outputs": [
          "Updated docs/specs without SQLite fallback instructions",
          "Status output that remains useful without durable local state"
        ],
        "verification": [
          "Docs no longer instruct operators to inspect `maestro.sqlite3` for normal recovery",
          "Status output still reports retained workspaces and active issue context in a reviewable way"
        ],
        "depends_on": [
          "recover-from-tracker-and-workspaces"
        ]
      },
      {
        "id": "verify-xy-124-delivery",
        "title": "Verify XY-124 end-to-end and prepare delivery",
        "status": "in_progress",
        "objective": "Finish the lane with repo-native verification, focused self-review, and delivery-ready evidence.",
        "inputs": [
          "Updated runtime store and recovery logic",
          "Updated docs/specs"
        ],
        "outputs": [
          "Passing repo-native verification evidence",
          "A reviewable XY-124 delivery commit sequence"
        ],
        "verification": [
          "Run the full verification command set from defaults.verification_commands",
          "Self-review the restart-recovery semantics before requesting external review"
        ],
        "depends_on": [
          "align-operator-surface-and-docs"
        ]
      }
    ],
    "replan_policy": {
      "owner": "plan-writing",
      "triggers": [
        "Daemon supervision still requires a cross-process active-child model that cannot share process-memory state cleanly",
        "Restart recovery requires tracker comment history or another tracker-visible surface beyond current issue refresh data",
        "Status requirements prove that operator visibility must split from the no-SQLite runtime transition"
      ]
    }
  },
  "state": {
    "phase": "executing",
    "current_task_id": "verify-xy-124-delivery",
    "next_task_id": "verify-xy-124-delivery",
    "blockers": [],
    "evidence": [
      "2026-03-17: XY-125, XY-126, and XY-128 are merged, so XY-124 is the next structural runtime lane.",
      "2026-03-17: Current `main` already uses clone-backed `.workspaces` lanes, so XY-141 is stale imported backlog context rather than a prerequisite for XY-124.",
      "2026-03-17: src/state.rs is now an in-memory runtime store and Cargo.toml/Cargo.lock no longer depend on rusqlite or SQLite support crates.",
      "2026-03-17: run-once, daemon, and status now recover retained workspaces and active-lane redispatch intent from current Linear state plus deterministic `.workspaces/<ISSUE>` inspection.",
      "2026-03-17: docs/spec/system_maestro_runtime.md and docs/guide/pilot.md now describe memory-only runtime state and remove SQLite recovery guidance.",
      "2026-03-17: Local verification is complete: cargo make lint-fix, cargo make fmt, cargo make test, cargo make lint, cargo make fmt-rust-check, and cargo make fmt-toml-check all pass."
    ],
    "last_updated": "2026-03-17T04:37:54Z",
    "replan_reason": null,
    "context_snapshot": {
      "active_structural_issue": "XY-124",
      "previous_structural_issue": "XY-128",
      "current_gap": "Implementation and local verification are complete; the remaining work is delivery preparation, external review, and merge.",
      "active_lane": "x/maestro-xy-124",
      "stale_issue_to_close": "XY-141"
    }
  }
}
```

# XY-124 Plan

This plan is the execution authority for `XY-124`.
