```json
{
  "spec": {
    "schema": "plan/1",
    "plan_id": "maestro-structural-runtime-followups-2026-03-13",
    "goal": "Stage the structural runtime follow-ups after Maestro already has a reviewable and observable self-supervision loop.",
    "success_criteria": [
      "The backlog preserves retry/backoff hardening ahead of remove-SQLite durability removal.",
      "PUB-610 has a clear execution shape, including whether it needs smaller child issues before live execution.",
      "PUB-608 remains intentionally downstream of the reviewable and observable self-supervision loop rather than being pulled forward prematurely.",
      "The plan captures the decision points required before removing SQLite durability without assuming implementation now."
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
      "first_structural_issue": "PUB-610",
      "second_structural_issue": "PUB-608",
      "verification_commands": [
        "Read the current Linear backlog state",
        "Confirm the self-supervision loop is already reviewable and observable before structural work starts"
      ]
    },
    "tasks": [
      {
        "id": "confirm-loop-readiness",
        "title": "Confirm the self-supervision loop is already reviewable and observable",
        "status": "pending",
        "objective": "Prevent retry and remove-SQLite work from entering the critical path before the base loop is stable enough to support them.",
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
        "id": "shape-pub-610",
        "title": "Shape retry and backoff work into an executable lane",
        "status": "pending",
        "objective": "Decide whether PUB-610 can be executed as one bounded issue or must be split before live execution.",
        "inputs": [
          "PUB-610",
          "Observed daemon pilot evidence",
          "Current runtime state model"
        ],
        "outputs": [
          "A bounded execution shape for retry and backoff work",
          "A decision on whether child issues are needed before live execution"
        ],
        "verification": [
          "The resulting scope is small enough for one live lane or explicitly split into smaller lanes",
          "The order still keeps retry work ahead of remove-SQLite"
        ],
        "depends_on": [
          "confirm-loop-readiness"
        ]
      },
      {
        "id": "shape-pub-608",
        "title": "Define the remove-SQLite transition boundary",
        "status": "pending",
        "objective": "Specify what evidence and runtime surfaces must already exist before SQLite durability can be removed safely.",
        "inputs": [
          "PUB-608",
          "Current runtime and observability surfaces",
          "Retry/backoff shaping outcome"
        ],
        "outputs": [
          "A clear transition boundary for remove-SQLite work",
          "A decision on whether PUB-608 also needs smaller child issues"
        ],
        "verification": [
          "The remove-SQLite scope stays behind retry/backoff and does not jump ahead of unresolved observability or reconciliation gaps"
        ],
        "depends_on": [
          "shape-pub-610"
        ]
      }
    ],
    "replan_policy": {
      "owner": "plan-writing",
      "triggers": [
        "The daemon supervision plan leaves unresolved manual-intervention gaps that make PUB-610 or PUB-608 unsafe to start",
        "Observed runtime behavior implies retry/backoff must be split more finely before any live execution",
        "Remove-SQLite work would invalidate the operator visibility or reconciliation assumptions established earlier"
      ]
    }
  },
  "state": {
    "phase": "ready",
    "current_task_id": null,
    "next_task_id": "confirm-loop-readiness",
    "blockers": [],
    "evidence": [
      "2026-03-13: PUB-610 and PUB-608 remain Icebox while the critical path stays on PR-backed handoff and daemon supervision.",
      "2026-03-13: The current stated order is PUB-610 before PUB-608, and PUB-608 should stay behind the reviewable and observable loop."
    ],
    "last_updated": "2026-03-13T11:32:19Z",
    "replan_reason": null,
    "context_snapshot": {
      "current_gap": "Retry/backoff and remove-SQLite work are intentionally staged later and still need explicit execution shaping before implementation.",
      "ordering_rule": "PUB-610 stays ahead of PUB-608"
    }
  }
}
```

# Structural Follow-Ups Plan

This plan exists so larger runtime changes stay intentionally sequenced after the self-supervision loop is already proven.
