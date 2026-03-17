```json
{
  "spec": {
    "schema": "plan/1",
    "plan_id": "maestro-xy-128-workflow-reload-last-known-good-2026-03-17",
    "goal": "Implement XY-128 by making daemon WORKFLOW.md reload keep the last known good repo policy active while future lanes pick up valid workflow changes without a process restart.",
    "success_criteria": [
      "Daemon mode reloads the configured repo-owned WORKFLOW.md on future ticks without requiring a process restart.",
      "If the configured WORKFLOW.md becomes invalid, the daemon keeps using the last known good workflow document for the same configured path instead of dropping the whole tick.",
      "A running child lane keeps the workflow snapshot it started with, so repo-policy edits do not destabilize an in-flight attempt.",
      "Future dispatch, retry, reconciliation after child exit, and prompt generation use the latest valid workflow document.",
      "Runtime docs, workflow contract docs, operator docs, and regression tests describe and enforce the reload semantics clearly."
    ],
    "constraints": [
      "Keep TOML frontmatter as the only machine-readable WORKFLOW.md format.",
      "Do not add a filesystem watcher; defensive per-tick reload is sufficient for this lane.",
      "Do not add new durable state; last-known-good workflow may remain daemon-memory only for this phase.",
      "Do not bundle remove-SQLite durability removal or broader config-reload work into XY-128."
    ],
    "defaults": {
      "authority_issue": "XY-128",
      "config_path": "./tmp/maestro.toml",
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
        "id": "codify-reload-contract",
        "title": "Codify daemon WORKFLOW reload semantics",
        "status": "in_progress",
        "objective": "Make the reload contract explicit in plans, specs, and operator docs before changing runtime behavior.",
        "inputs": [
          "XY-128 issue scope",
          "Current per-tick daemon context loading",
          "Current workflow and runtime specifications"
        ],
        "outputs": [
          "A dedicated XY-128 execution plan",
          "Normative docs that describe same-path last-known-good fallback and active-lane stability"
        ],
        "verification": [
          "Docs describe what reload affects immediately versus what stays stable for the running child",
          "The structural plan now points at XY-128 instead of the already merged XY-126 lane"
        ],
        "depends_on": []
      },
      {
        "id": "implement-daemon-workflow-cache",
        "title": "Implement daemon-side last-known-good workflow caching",
        "status": "pending",
        "objective": "Teach daemon mode to reuse the last valid workflow document when same-path reloads fail and to swap in new valid documents on later ticks.",
        "inputs": [
          "Current load_daemon_tick_context flow",
          "Configured repo_root and workflow_path",
          "Codified reload contract"
        ],
        "outputs": [
          "A daemon-memory cache of the last valid workflow document for the configured path",
          "Reload handling that logs and keeps scheduling with the cached workflow when a same-path parse fails"
        ],
        "verification": [
          "Tests cover initial valid load, invalid same-path reload fallback, and later valid reload replacement",
          "Daemon tick loading still fails fast when no prior valid workflow exists"
        ],
        "depends_on": [
          "codify-reload-contract"
        ]
      },
      {
        "id": "preserve-active-lane-stability",
        "title": "Keep active child lanes stable across workflow edits",
        "status": "pending",
        "objective": "Ensure an already running child continues under the workflow snapshot it started with while future decisions use the latest valid document.",
        "inputs": [
          "Daemon child lifecycle",
          "Reloaded workflow cache",
          "Current active-run reconciliation path"
        ],
        "outputs": [
          "A workflow snapshot carried with the active child lane",
          "Reconciliation logic that does not reclassify an in-flight child based on mid-run workflow edits"
        ],
        "verification": [
          "Regression coverage proves active-lane reconciliation keeps the spawn-time workflow while child exit follow-up uses the current good workflow",
          "Prompt generation for later attempts reads the latest valid workflow"
        ],
        "depends_on": [
          "implement-daemon-workflow-cache"
        ]
      },
      {
        "id": "verify-xy-128-delivery",
        "title": "Verify XY-128 end-to-end and prepare delivery",
        "status": "pending",
        "objective": "Finish the lane with repo-native verification, self-review, and delivery-ready evidence.",
        "inputs": [
          "Updated daemon reload behavior",
          "Updated specs and operator docs",
          "Regression tests for reload semantics"
        ],
        "outputs": [
          "Passing repo-native verification evidence",
          "A reviewable XY-128 delivery commit sequence"
        ],
        "verification": [
          "Run the full verification command set from defaults.verification_commands",
          "Self-review the reload and active-lane semantics before requesting external review"
        ],
        "depends_on": [
          "preserve-active-lane-stability"
        ]
      }
    ],
    "replan_policy": {
      "owner": "plan-writing",
      "triggers": [
        "Reload semantics require a broader service-config cache beyond repo-owned WORKFLOW.md",
        "Active-lane stability cannot be expressed cleanly without splitting child-reconciliation work from workflow-cache work",
        "Last-known-good fallback would require new durable persistence before XY-124 intentionally removes SQLite"
      ]
    }
  },
  "state": {
    "phase": "executing",
    "current_task_id": "codify-reload-contract",
    "next_task_id": "codify-reload-contract",
    "blockers": [],
    "evidence": [
      "2026-03-17: XY-128 is the next structural runtime issue after XY-126 merged and closed.",
      "2026-03-17: Daemon mode currently reloads ServiceConfig and WORKFLOW.md on each tick through load_daemon_tick_context, but an invalid WORKFLOW.md parse aborts that tick instead of keeping the last known good workflow active.",
      "2026-03-17: run --once already reads WORKFLOW.md per invocation, so the missing behavior is daemon-side last-known-good fallback rather than one-shot workflow freshness.",
      "2026-03-17: Active-child reconciliation currently consumes the current tick workflow context, so mid-run repo-policy edits can affect an already running lane unless the child carries its own workflow snapshot.",
      "2026-03-17: XY-128 acceptance requires future daemon decisions to pick up valid repo-owned workflow changes without restart while invalid reloads keep the last known good workflow active and in-flight runs remain stable."
    ],
    "last_updated": "2026-03-17T03:20:00Z",
    "replan_reason": null,
    "context_snapshot": {
      "active_structural_issue": "XY-128",
      "previous_structural_issue": "XY-126",
      "current_reload_gap": "Daemon ticks drop the cycle on invalid WORKFLOW.md reloads and do not preserve a child-specific workflow snapshot for active-lane stability.",
      "active_lane": "x/maestro-xy-128"
    }
  }
}
```

# XY-128 Plan

This plan is the execution authority for `XY-128`.
