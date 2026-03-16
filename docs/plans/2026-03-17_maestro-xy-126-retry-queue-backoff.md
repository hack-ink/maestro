```json
{
  "spec": {
    "schema": "plan/1",
    "plan_id": "maestro-xy-126-retry-queue-backoff-2026-03-17",
    "goal": "Implement XY-126 by adding an explicit in-memory retry queue, short continuation retries, and capped exponential failure backoff to the Maestro daemon.",
    "success_criteria": [
      "The daemon owns an explicit retry entry model instead of relying on comment-only retry intent.",
      "Continuation retries after a clean worker exit are handled separately from failure-driven retries.",
      "Failure-driven retries use exponential backoff with a repository-owned cap in WORKFLOW.md.",
      "Retry handling revalidates the queued issue before redispatch and releases the queued claim when the issue is missing, terminal, or otherwise non-active.",
      "Runtime/docs/tests stay aligned for the new retry semantics."
    ],
    "constraints": [
      "Do not add new durable retry persistence; keep retry queue state in daemon memory for this phase.",
      "Keep the current TOML WORKFLOW.md contract and extend it only when repo-owned execution policy needs a new field.",
      "Do not bundle SSH worker routing or the remove-SQLite transition into this lane.",
      "Preserve the existing single-dispatch-slot model introduced by XY-125."
    ],
    "defaults": {
      "authority_issue": "XY-126",
      "config_path": "./tmp/maestro.toml",
      "continuation_retry_delay_ms": 1000,
      "failure_retry_base_delay_ms": 10000,
      "failure_retry_cap_ms_field": "execution.max_retry_backoff_ms",
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
        "id": "update-retry-contract-surface",
        "title": "Extend the repo-owned retry contract surface",
        "status": "done",
        "objective": "Add the workflow/runtime contract needed to express capped failure backoff without changing the broader structural sequence.",
        "inputs": [
          "XY-126 issue scope",
          "Current WORKFLOW.md execution contract",
          "Current runtime/spec docs"
        ],
        "outputs": [
          "A parsed WORKFLOW execution field for the retry backoff cap",
          "Normative docs that explain continuation retry versus failure retry"
        ],
        "verification": [
          "Workflow parsing tests cover the new execution field",
          "Docs describe the cap and the two retry paths without contradicting current daemon ownership"
        ],
        "depends_on": []
      },
      {
        "id": "add-in-memory-retry-queue",
        "title": "Add explicit in-memory retry entries to daemon runtime",
        "status": "done",
        "objective": "Model queued retries directly in the daemon so retry intent survives across poll ticks without introducing new durable storage.",
        "inputs": [
          "Current run_daemon loop",
          "Existing single-slot claim behavior from XY-125",
          "Updated retry contract surface"
        ],
        "outputs": [
          "Retry entry and queue types owned by the daemon",
          "Scheduling helpers for continuation and failure retry delays"
        ],
        "verification": [
          "Helpers cover fixed continuation delay and capped exponential failure backoff",
          "Queue behavior replaces existing entries per issue instead of duplicating retries"
        ],
        "depends_on": [
          "update-retry-contract-surface"
        ]
      },
      {
        "id": "wire-retry-dispatch-and-release",
        "title": "Wire retry queue dispatch and claim release into daemon flow",
        "status": "done",
        "objective": "Make daemon child exits and retry timers feed the queue, redispatch queued issues before normal selection, and drop queued claims when the issue is no longer active.",
        "inputs": [
          "Retry queue runtime types",
          "Current daemon child lifecycle and reconciliation flow",
          "Current issue eligibility helpers"
        ],
        "outputs": [
          "Worker-exit scheduling for continuation and failure retries",
          "Retry-timer dispatch that revalidates issue activity before redispatch",
          "Claim release when the queued issue is missing, terminal, needs attention, or otherwise non-active"
        ],
        "verification": [
          "Tests cover normal-exit continuation retry scheduling, failure retry backoff scheduling, and queued-claim release on non-active issues",
          "Retry dispatch keeps normal candidate selection behind queued claims in the single-slot runtime"
        ],
        "depends_on": [
          "add-in-memory-retry-queue"
        ]
      },
      {
        "id": "verify-xy-126-delivery",
        "title": "Verify XY-126 end-to-end and prepare delivery",
        "status": "in_progress",
        "objective": "Finish the lane with repo-native verification and updated execution evidence.",
        "inputs": [
          "Implemented retry queue and backoff semantics",
          "Updated plan/runtime docs"
        ],
        "outputs": [
          "Passing repo-native verification evidence",
          "A reviewable XY-126 delivery commit sequence"
        ],
        "verification": [
          "Run the full verification command set from defaults.verification_commands",
          "Self-review the retry paths before requesting external review"
        ],
        "depends_on": [
          "wire-retry-dispatch-and-release"
        ]
      }
    ],
    "replan_policy": {
      "owner": "plan-writing",
      "triggers": [
        "The runtime needs broader concurrency or claimed-set changes beyond the single-slot model already landed in XY-125",
        "Retry queue semantics require new durable persistence before XY-124 intentionally removes SQLite",
        "Daemon child exit behavior proves too entangled with reconciliation and the lane must split into a queue task plus a follow-up recovery task"
      ]
    }
  },
  "state": {
    "phase": "executing",
    "current_task_id": "verify-xy-126-delivery",
    "next_task_id": "verify-xy-126-delivery",
    "blockers": [],
    "evidence": [
      "2026-03-17: XY-126 is the next structural runtime issue after XY-125 merged and closed.",
      "2026-03-17: Current runtime only exposes execution.max_attempts plus retry comments; there is no explicit retry queue or retry backoff config field.",
      "2026-03-17: Current daemon behavior spawns at most one child, and retry intent is not persisted across poll ticks except implicitly through tracker state and comments.",
      "2026-03-17: Symphony SPEC sections 7.3 and 8.4 define the target semantics: clean worker exits schedule a short continuation retry, abnormal exits schedule exponential backoff, and retry firing revalidates the issue before redispatch or claim release.",
      "2026-03-17: Runtime implementation now owns an explicit in-memory RetryQueue plus retry dispatch helpers, run-once issue override support, and retry-policy revalidation for active `In Progress` issues.",
      "2026-03-17: Repo-owned contract docs now expose execution.max_retry_backoff_ms and describe continuation retry versus capped failure retry semantics in WORKFLOW.md, workflow spec, runtime spec, and pilot guide.",
      "2026-03-17: Local verification passed for cargo make lint-fix/fmt/test/lint/fmt-rust-check/fmt-toml-check and git diff --check after adding retry-queue regression coverage for continuation scheduling, failure scheduling, queued-claim release, and queued-claim blocking before due time."
    ],
    "last_updated": "2026-03-16T18:08:15Z",
    "replan_reason": null,
    "context_snapshot": {
      "active_structural_issue": "XY-126",
      "previous_structural_issue": "XY-125",
      "single_slot_runtime": true,
      "current_retry_gap": "Local implementation and verification are complete; the remaining gap is delivery, review, and tracker closeout.",
      "active_lane": "x/maestro-xy-126"
    }
  }
}
```

# XY-126 Plan

This plan is the execution authority for `XY-126`.
