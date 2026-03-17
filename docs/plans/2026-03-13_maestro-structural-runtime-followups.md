```json
{
  "spec": {
    "schema": "plan/1",
    "plan_id": "maestro-structural-runtime-followups-2026-03-13",
    "goal": "Stage the post-pilot runtime hardening backlog after Maestro already has a reviewable and observable self-supervision loop.",
    "success_criteria": [
      "The backlog preserves claim/concurrency policy ahead of retry/backoff, explicit reload semantics ahead of remove-SQLite durability removal, and remove-SQLite last.",
      "XY-125 has a clear execution shape, including candidate claims, concurrency ceilings, and blocker gating.",
      "XY-126 has a clear execution shape for retry queue and exponential backoff once XY-125 is explicit.",
      "XY-128 has an explicit slot in the sequence instead of remaining an implicit contract gap.",
      "XY-124 remains intentionally downstream of the reviewable and observable self-supervision loop rather than being pulled forward prematurely."
    ],
    "constraints": [
      "Assume PR-backed handoff, daemon reconciliation, and operator visibility are already in place before executing this plan.",
      "Do not implement these structural follow-ups as part of planning; this plan only stages and sequences them.",
      "Keep Linear as the durable workflow database of record.",
      "Do not introduce SSH worker execution in this phase."
    ],
    "defaults": {
      "config_path": "./tmp/maestro.toml",
      "execution_project": "Maestro Pilot Ops Hardening",
      "candidate_policy_issue": "XY-125",
      "retry_issue": "XY-126",
      "reload_contract_issue": "XY-128",
      "remove_sqlite_issue": "XY-124",
      "verification_commands": [
        "Read the current Linear backlog state",
        "Confirm the self-supervision loop is already reviewable and observable before structural work starts"
      ]
    },
    "tasks": [
      {
        "id": "confirm-loop-readiness",
        "title": "Confirm the self-supervision loop is already reviewable and observable",
        "status": "done",
        "objective": "Prevent post-pilot runtime hardening from entering the critical path before the base loop is stable enough to support it.",
        "inputs": [
          "Outcomes of the PR-backed handoff plan",
          "Outcomes of the daemon supervision plan"
        ],
        "outputs": [
          "A confirmed precondition that the loop is already reviewable, reconciled, and operator-visible"
        ],
        "verification": [
          "Read the merged outcomes for the earlier plans",
          "Confirm the current loop no longer depends on manual PR creation or ad hoc SQL"
        ],
        "depends_on": []
      },
      {
        "id": "shape-xy-125",
        "title": "Shape claim, concurrency, and blocker-aware candidate policy",
        "status": "done",
        "objective": "Decide whether XY-125 can be executed as one bounded issue or must be split before live execution.",
        "inputs": [
          "XY-125",
          "Observed daemon pilot evidence",
          "Current candidate selection and workspace claim logic"
        ],
        "outputs": [
          "A bounded execution shape for claim/concurrency policy work",
          "Clear acceptance criteria for claimed-set awareness, concurrency ceilings, and blocker gating"
        ],
        "verification": [
          "The resulting scope is small enough for one live lane or explicitly split into smaller lanes",
          "The order still keeps XY-125 ahead of retry/backoff and remove-SQLite"
        ],
        "depends_on": [
          "confirm-loop-readiness"
        ]
      },
      {
        "id": "shape-xy-126",
        "title": "Shape retry queue and backoff work into an executable lane",
        "status": "done",
        "objective": "Decide whether XY-126 can be executed as one bounded issue after XY-125 or must be split before live execution.",
        "inputs": [
          "XY-126",
          "Observed daemon pilot evidence",
          "The shaped XY-125 contract"
        ],
        "outputs": [
          "A bounded execution shape for retry queue and exponential backoff work",
          "A decision on whether child issues are needed before live execution"
        ],
        "verification": [
          "The resulting scope is small enough for one live lane or explicitly split into smaller lanes",
          "The order still keeps XY-126 behind XY-125 and ahead of XY-124"
        ],
        "depends_on": [
          "shape-xy-125"
        ]
      },
      {
        "id": "place-xy-128",
        "title": "Place WORKFLOW reload semantics in the post-pilot sequence",
        "status": "in_progress",
        "objective": "Define whether XY-128 should land as a small standalone contract task or fold into adjacent runtime hardening without becoming implicit.",
        "inputs": [
          "XY-128",
          "Current per-tick config and WORKFLOW reload behavior",
          "The shaped XY-125 and XY-126 sequence"
        ],
        "outputs": [
          "An explicit execution slot for last-known-good reload semantics",
          "A decision on whether XY-128 is standalone or bundled with adjacent work"
        ],
        "verification": [
          "Reload semantics are explicit before remove-SQLite work starts",
          "No hidden contract drift remains between docs and runtime behavior"
        ],
        "depends_on": [
          "shape-xy-126"
        ]
      },
      {
        "id": "shape-xy-124",
        "title": "Define the remove-SQLite transition boundary",
        "status": "pending",
        "objective": "Specify what evidence and runtime surfaces must already exist before SQLite durability can be removed safely.",
        "inputs": [
          "XY-124",
          "Current runtime and observability surfaces",
          "The shaped XY-126 and XY-128 outcomes"
        ],
        "outputs": [
          "A clear transition boundary for remove-SQLite work",
          "A decision on whether XY-124 also needs smaller child issues"
        ],
        "verification": [
          "The remove-SQLite scope stays behind XY-125, XY-126, and XY-128 and does not jump ahead of unresolved observability or reconciliation gaps"
        ],
        "depends_on": [
          "place-xy-128"
        ]
      }
    ],
    "replan_policy": {
      "owner": "plan-writing",
      "triggers": [
        "The daemon supervision plan leaves unresolved manual-intervention gaps that make XY-125, XY-126, XY-128, or XY-124 unsafe to start",
        "Observed runtime behavior implies claim/concurrency or retry/backoff work must be split more finely before any live execution",
        "Remove-SQLite work would invalidate the operator visibility or reconciliation assumptions established earlier"
      ]
    }
  },
  "state": {
    "phase": "executing",
    "current_task_id": "place-xy-128",
    "next_task_id": "place-xy-128",
    "blockers": [],
    "evidence": [
      "2026-03-13: PUB-610 and PUB-608 were originally staged after PR-backed handoff and daemon supervision so structural work would not enter the critical path too early.",
      "2026-03-16: The current hackink backlog has expanded the post-pilot runtime set to XY-125, XY-126, XY-128, and XY-124.",
      "2026-03-16: Fresh daemon follow-up XY-143 originally kept this structural plan staged behind daemon supervision.",
      "2026-03-16: PR #7 merged XY-143, so loop readiness is now confirmed: the current self-supervision path is PR-backed, reconciled, operator-visible, and no longer depends on manual PR creation or ad hoc SQL.",
      "2026-03-17: XY-125 is now done after PR #8 fast-forwarded onto main, so claim/concurrency/blocker policy is no longer the active structural gap.",
      "2026-03-17: XY-126 is now done after PR #9 fast-forwarded onto main, so retry queue and backoff semantics are no longer the active structural gap.",
      "2026-03-17: XY-128 is now the active structural execution lane via docs/plans/2026-03-17_maestro-xy-128-workflow-reload-last-known-good.md, with XY-124 still intentionally last.",
      "2026-03-16: XY-141 should not block this plan because current `main` already uses clone-backed `.workspaces` lanes and the imported issue description is stale."
    ],
    "last_updated": "2026-03-17T03:20:00Z",
    "replan_reason": null,
    "context_snapshot": {
      "current_gap": "The next remaining runtime gap is XY-128 last-known-good WORKFLOW reload semantics; daemon supervision, XY-125, and XY-126 are complete.",
      "ordering_rule": "XY-125 stays ahead of XY-126, XY-128 is placed explicitly before XY-124, and XY-124 stays last.",
      "next_candidate_after_daemon_closeout": "XY-128"
    }
  }
}
```

# Structural Follow-Ups Plan

This plan exists so larger runtime changes stay intentionally sequenced after the self-supervision loop is already proven.
