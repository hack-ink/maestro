```json
{
  "spec": {
    "schema": "plan/1",
    "plan_id": "maestro-daemon-supervision-2026-03-13",
    "goal": "Make Maestro capable of supervising its own development through a daemon-driven, reconciled, and operator-visible loop after PR-backed handoff exists.",
    "success_criteria": [
      "XY-127 is merged and active runs reconcile correctly on each daemon tick.",
      "XY-129 is merged and operators can inspect current run state without ad hoc SQL or source reading.",
      "One fresh daemon-driven live lane completes with PR-backed handoff, clean reconciliation, and usable operator visibility.",
      "Any remaining manual intervention points are written down as explicit follow-up gaps rather than left implicit."
    ],
    "constraints": [
      "Assume XY-134 is already merged before execution begins.",
      "Keep Linear as the durable workflow tracker.",
      "Do not introduce SQLite as a durable operator store.",
      "Tune stall and liveness logic from app-server schema, live telemetry, and upstream implementation evidence, not local timing guesses alone.",
      "Do not widen this plan into retry/backoff redesign or remove-SQLite work."
    ],
    "defaults": {
      "config_path": "./tmp/maestro.toml",
      "daemon_command": "cargo run -- daemon --poll-interval-s 5 --config ./tmp/maestro.toml",
      "execution_project": "Maestro Pilot Ops Hardening",
      "observability_issue": "XY-129",
      "preflight_commands": [
        "cargo run -- protocol probe",
        "cargo run -- run --once --dry-run --config ./tmp/maestro.toml"
      ],
      "prerequisite_issue": "XY-134",
      "primary_issue": "XY-127",
      "verification_commands": [
        "cargo make fmt-check",
        "cargo make lint",
        "cargo make test"
      ]
    },
    "tasks": [
      {
        "id": "confirm-pr-backed-prerequisite",
        "title": "Confirm PR-backed handoff is available before daemon supervision work",
        "status": "done",
        "objective": "Prevent daemon supervision work from validating against a stale manual review contract.",
        "inputs": [
          "XY-134 outcome",
          "Current runtime docs and main branch state"
        ],
        "outputs": [
          "Confirmed prerequisite that successful runs now yield PR-backed In Review handoff"
        ],
        "verification": [
          "Read the merged XY-134 outcome and updated runtime docs"
        ],
        "depends_on": []
      },
      {
        "id": "implement-pub-611",
        "title": "Implement per-tick stall detection and active-run reconciliation",
        "status": "done",
        "objective": "Reduce manual cleanup by making each daemon tick capable of recognizing stalled or non-active lanes and converging local state.",
        "inputs": [
          "XY-127 (historical PUB-611)",
          "docs/spec/system_maestro_runtime.md",
          "docs/spec/system_app_server_contract.md",
          "Relevant Codex app-server implementation evidence for waiting and terminal semantics"
        ],
        "outputs": [
          "Merged per-tick stall-detection and reconciliation behavior",
          "Tests and docs that define the expected tick semantics"
        ],
        "verification": [
          "cargo make fmt-check",
          "cargo make lint",
          "cargo make test"
        ],
        "depends_on": [
          "confirm-pr-backed-prerequisite"
        ]
      },
      {
        "id": "implement-pub-613",
        "title": "Add the minimal operator visibility surface",
        "status": "done",
        "objective": "Expose current run state through structured logs and a small status surface so daemon behavior can be supervised directly.",
        "inputs": [
          "XY-129 (historical PUB-613)",
          "Protocol events already recorded by the runtime",
          "Merged XY-127 behavior"
        ],
        "outputs": [
          "Structured runtime logs with stable identifiers",
          "A minimal status interface such as maestro status or equivalent JSON output",
          "Updated docs describing the supported live visibility surface"
        ],
        "verification": [
          "cargo make fmt-check",
          "cargo make lint",
          "cargo make test",
          "Demonstrate that current run state can be inspected without ad hoc SQL"
        ],
        "depends_on": [
          "implement-pub-611"
        ]
      },
      {
        "id": "run-daemon-pilot",
        "title": "Run one daemon-driven self-supervision pilot on a fresh bounded issue",
        "status": "in_progress",
        "objective": "Prove that Maestro can supervise a bounded internal issue through the daemon path once reconciliation and visibility exist.",
        "inputs": [
          "Merged XY-127 and XY-129",
          "One fresh bounded Todo issue in the hardening project",
          "./tmp/maestro.toml"
        ],
        "outputs": [
          "One daemon-driven live lane with observed selection, reconciliation, PR-backed handoff, and operator status evidence",
          "A concise list of any remaining operator-only intervention points"
        ],
        "verification": [
          "cargo run -- protocol probe",
          "cargo run -- run --once --dry-run --config ./tmp/maestro.toml",
          "cargo run -- daemon --poll-interval-s 5 --config ./tmp/maestro.toml",
          "Inspect the resulting issue, branch, PR, status output, and workspace lifecycle"
        ],
        "depends_on": [
          "implement-pub-613"
        ]
      }
    ],
    "replan_policy": {
      "owner": "plan-writing",
      "triggers": [
        "XY-127 reveals that stall semantics depend on upstream behavior not yet inspected",
        "XY-129 needs to be split before a minimal status surface can land cleanly",
        "The first daemon pilot shows that a smaller child issue is required before retry work begins"
      ]
    }
  },
  "state": {
    "phase": "executing",
    "current_task_id": "run-daemon-pilot",
    "next_task_id": "run-daemon-pilot",
    "blockers": [],
    "evidence": [
      "2026-03-13: Current runtime guidance already states that stall and liveness policy must use schema, live telemetry, and upstream implementation evidence together.",
      "2026-03-13: Protocol telemetry exists, but the operator-facing status surface is not yet implemented as a supported interface.",
      "2026-03-13: Backlog order currently places PUB-611 before PUB-613 before any daemon self-supervision pilot.",
      "2026-03-14: Confirmed on origin/main at 90e69adf29ea3d2901399d12be58e1990d4b0206 that PUB-618 landed the PR-backed In Review contract in the runtime and tracker-tool specs.",
      "2026-03-14: Opened clean execution lane x/maestro-pub-611 at .workspaces/PUB-611 from origin/main for the next daemon-supervision task.",
      "2026-03-14: Implemented PUB-611 on branch x/maestro-pub-611, verified with cargo make lint-fix/fmt/test, opened PR #10 at https://github.com/helixbox/maestro/pull/10, and requested `@codex review` at https://github.com/helixbox/maestro/pull/10#issuecomment-4060162812.",
      "2026-03-14: Addressed the first PR #10 review round in commit 7c6d3a68e8d1f98d59cc1b9b838efc09d02751aa by deferring non-active interruption for startable pre-claim states, reconciling exited-child stalled runs from protocol activity, rerunning cargo make lint-fix/fmt/test, replying in both inline threads, resolving both review threads, and requesting another `@codex review` at https://github.com/helixbox/maestro/pull/10#issuecomment-4060190690.",
      "2026-03-14: PR #10 was merged into `main`; `origin/main` now fast-forwards to 7c6d3a68e8d1f98d59cc1b9b838efc09d02751aa, preserving the delivery/1 commit-message contract on both PUB-611 commits, and Linear issue PUB-611 was closed as Done.",
      "2026-03-14: Implemented PUB-613 on branch x/maestro-pub-613, verified with cargo make fmt-check/lint/test plus live `cargo run -- status` text and JSON reads against ./tmp/maestro.toml, opened PR #11 at https://github.com/helixbox/maestro/pull/11, moved Linear issue PUB-613 to In Review, and requested `@codex review` after an explicit self-review.",
      "2026-03-14: Addressed the first PR #11 review round in commit 3edb7b3c29a61eec49fa75492279379e939b911d by separating uncapped active-run status from limit-bound recent runs, rerunning cargo make fmt-check/lint/test, replying in the inline thread, and resolving that review thread.",
      "2026-03-14: PR #11 was merged into `main` by fast-forward push; `origin/main` now points at 3edb7b3c29a61eec49fa75492279379e939b911d with the reviewed `delivery/1` commit messages preserved, and Linear issue PUB-613 was closed as Done.",
      "2026-03-14: Opened PUB-622 as the operator-side pilot tracker for the first daemon-driven self-supervision run after merged PUB-611 and PUB-613.",
      "2026-03-14: Opened PUB-623 as the actual fresh bounded Todo issue that the daemon should select for the first live self-supervision lane.",
      "2026-03-14: Created a clean detached runner checkout at `tmp/maestro-runner` on origin/main, validated `cargo run -- protocol probe`, and confirmed `cargo run -- run --once --dry-run --config ./tmp/maestro.toml` selects PUB-623 on branch x/maestro-pub-623 with runner-local workspace path `tmp/maestro-runner/.workspaces/PUB-623`.",
      "2026-03-14: Refreshed the runner checkout to current origin/main at 97a17534073497cb8f24d0ef636be4e429afca54, reran `cargo run -- protocol probe`, and reran `cargo run -- run --once --dry-run --config ./tmp/maestro.toml`; both passed and still targeted PUB-623.",
      "2026-03-14: The live daemon pilot selected PUB-623, created `.workspaces/PUB-623`, wrote `Started work` comments for attempts 1 and 2, and exposed live state through `maestro status --json` without any ad hoc SQL.",
      "2026-03-14: Attempts 1 and 2 both terminated with `error_class: stalled_run_detected`, but their failure comments reported impossible elapsed values (`18446744073709551615s` and `18446744073709551603s`) and the daemon still auto-started the next attempt instead of stopping after the `needs attention` terminal failure path.",
      "2026-03-14: After interrupting the daemon, runner-local `maestro status --json` still showed attempt 3 (`pub-623-attempt-3-1773493825`) as an active leased run, while the workspace remained diff-free and no PR-backed handoff or code changes were produced.",
      "2026-03-14: Created Linear label `maestro:needs-attention`, applied it to PUB-623, and opened follow-up issue PUB-625 to harden stalled-run duration calculation, retry suppression after `needs attention`, and stale active-lease cleanup before another daemon pilot.",
      "2026-03-14: Implemented PUB-625 on branch x/maestro-pub-625 in commit e95b47249f877762a7f17285c8d0d35a08b1e249, verified with cargo make lint-fix/fmt/lint/test, opened PR #12 at https://github.com/helixbox/maestro/pull/12, moved PUB-625 to In Review, and requested `@codex review` after a focused self-review.",
      "2026-03-14: PR #12 merged by fast-forward push; `origin/main` now points at e95b47249f877762a7f17285c8d0d35a08b1e249 with the reviewed `delivery/1` commit message preserved, and Linear issue PUB-625 is Done.",
      "2026-03-14: Refreshed `tmp/maestro-runner` to origin/main at e95b47249f877762a7f17285c8d0d35a08b1e249, retained PUB-623 for forensics under `maestro:needs-attention`, opened fresh seed issue PUB-626, reran `cargo run -- protocol probe`, and confirmed `cargo run -- run --once --dry-run --config ./tmp/maestro.toml` now selects PUB-626 on branch x/maestro-pub-626.",
      "2026-03-14: Started the rerun daemon pilot from `tmp/maestro-runner` with `cargo run -- daemon --poll-interval-s 5 --config ./tmp/maestro.toml`; `maestro status --json` shows active run `pub-626-attempt-1-1773496583`, Linear issue PUB-626 moved to In Progress with a start comment, and the runner workspace now contains README.md plus docs/guide/pilot.md edits while the lane remains active.",
      "2026-03-16: Verified the current repo routing is `y/hackink`; the PUB-era helixbox evidence above is retained as historical provenance only and no longer describes the active tracker authority.",
      "2026-03-16: The imported prerequisite set is satisfied in hackink with XY-134 Done (historical PUB-618), XY-127 Done (historical PUB-611), XY-129 Done (historical PUB-613), and XY-139 Done (historical PUB-625).",
      "2026-03-16: The carried-forward daemon pilot itself remains open on XY-136, while XY-137 is still Todo with `maestro:needs-attention` and XY-140 is still In Progress with `maestro:needs-attention`.",
      "2026-03-16: Current `main` already uses clone-backed `.workspaces` lanes, so XY-141's linked-worktree issue description is stale backlog context rather than a missing prerequisite for this subplan."
    ],
    "last_updated": "2026-03-16T08:21:28Z",
    "replan_reason": null,
    "context_snapshot": {
      "current_gap": "The daemon pilot authority now lives in hackink on XY-136. This subplan cannot close until that imported pilot is reconciled, the overlapping docs-seed issues XY-137 and XY-140 are consolidated, and checked-in pilot config/docs stop pointing at old helixbox slugs.",
      "required_prerequisite": "Merged XY-134 (historical PUB-618)",
      "active_lane": null,
      "active_workspace": null,
      "confirmed_main_head": "70f8b283933ea673f275ec62db3ea3e83f59ccb3",
      "pilot_tracker_issue": "XY-136",
      "pilot_tracker_issue_url": null,
      "pilot_seed_issue": null,
      "pilot_seed_issue_url": null,
      "pilot_runner_root": null,
      "active_pr": null,
      "active_commit": "70f8b283933ea673f275ec62db3ea3e83f59ccb3",
      "followup_issue": "XY-139",
      "followup_issue_url": null,
      "open_docs_seed_issues": [
        "XY-137",
        "XY-140"
      ],
      "stale_backlog_issue": "XY-141"
    }
  }
}
```

# Daemon Supervision Plan

This plan starts only after PR-backed handoff exists. It covers the first daemon-quality supervision loop and stops before retry/backoff or remove-SQLite work.
