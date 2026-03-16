```json
{
  "spec": {
    "schema": "plan/1",
    "plan_id": "maestro-self-bootstrap-program-2026-03-13",
    "goal": "Coordinate the split planning and execution sequence that moves Maestro toward a self-supervising internal development loop.",
    "success_criteria": [
      "The self-bootstrap strategy is split into narrower authoritative plans instead of one broad mixed-scope document.",
      "There is a single unambiguous execution order across the split plans: PR-backed handoff, daemon supervision, then structural runtime follow-ups.",
      "Each subplan has its own valid plan/1 contract and stable file path for execution."
    ],
    "constraints": [
      "This umbrella plan is coordination-only; execution authority for each phase lives in the referenced subplan file.",
      "Keep WORKFLOW.md on TOML frontmatter.",
      "Keep Linear as the durable workflow tracker of record.",
      "Do not collapse the split plans back into one mixed-scope execution plan unless plan-writing explicitly replans."
    ],
    "defaults": {
      "execution_project": "Maestro Pilot Ops Hardening",
      "phase_order": [
        "pr-backed-handoff-plan",
        "daemon-supervision-plan",
        "structural-followups-plan"
      ],
      "subplans": [
        "docs/plans/2026-03-13_maestro-pr-backed-handoff.md",
        "docs/plans/2026-03-13_maestro-daemon-supervision.md",
        "docs/plans/2026-03-13_maestro-structural-runtime-followups.md"
      ]
    },
    "tasks": [
      {
        "id": "publish-split-plans",
        "title": "Publish the split self-bootstrap plans",
        "status": "done",
        "objective": "Replace the earlier single mixed-scope plan with narrower plan/1 files that each own one execution phase.",
        "inputs": [
          "The earlier combined self-bootstrap plan",
          "Current hardening backlog and runtime docs"
        ],
        "outputs": [
          "A PR-backed handoff subplan",
          "A daemon supervision subplan",
          "A structural follow-ups subplan"
        ],
        "verification": [
          "Each subplan file exists and validates as plan/1"
        ],
        "depends_on": []
      },
      {
        "id": "pr-backed-handoff-plan",
        "title": "Execute the PR-backed handoff subplan",
        "status": "done",
        "objective": "Use the dedicated subplan to land the PR-backed handoff phase before later phases start.",
        "inputs": [
          "docs/plans/2026-03-13_maestro-pr-backed-handoff.md"
        ],
        "outputs": [
          "Merged PR-backed handoff contract",
          "One fresh validation lane ready for the next phase"
        ],
        "verification": [
          "Subplan completion criteria are met"
        ],
        "depends_on": [
          "publish-split-plans"
        ]
      },
      {
        "id": "daemon-supervision-plan",
        "title": "Execute the daemon supervision subplan",
        "status": "in_progress",
        "objective": "Use the dedicated subplan to land daemon reconciliation and operator visibility before structural changes begin.",
        "inputs": [
          "docs/plans/2026-03-13_maestro-daemon-supervision.md"
        ],
        "outputs": [
          "A daemon-capable self-supervision pilot with bounded operator intervention"
        ],
        "verification": [
          "Subplan completion criteria are met"
        ],
        "depends_on": [
          "pr-backed-handoff-plan"
        ]
      },
      {
        "id": "structural-followups-plan",
        "title": "Execute the structural follow-ups subplan",
        "status": "pending",
        "objective": "Use the dedicated subplan to stage claim policy, retry/backoff, reload semantics, and remove-SQLite work only after the earlier phases are proven.",
        "inputs": [
          "docs/plans/2026-03-13_maestro-structural-runtime-followups.md"
        ],
        "outputs": [
          "A sequenced structural backlog that preserves claim policy before retry/backoff, keeps reload semantics explicit, and leaves remove-SQLite last"
        ],
        "verification": [
          "Subplan completion criteria are met"
        ],
        "depends_on": [
          "daemon-supervision-plan"
        ]
      }
    ],
    "replan_policy": {
      "owner": "plan-writing",
      "triggers": [
        "A subplan proves too broad and must be split again",
        "The intended phase order changes because new runtime evidence changes the critical path",
        "One subplan becomes blocked by prerequisites that are not represented in the current split"
      ]
    }
  },
  "state": {
    "phase": "executing",
    "current_task_id": "daemon-supervision-plan",
    "next_task_id": "daemon-supervision-plan",
    "blockers": [],
    "evidence": [
        "2026-03-13: The earlier combined self-bootstrap plan mixed PR-backed handoff, daemon supervision, and later structural changes in one contract.",
        "2026-03-13: The user requested that the work be split across multiple plans.",
        "2026-03-13: Execution moved into the PR-backed handoff subplan as the first sequential phase.",
        "2026-03-13: PUB-618 is implemented on branch x/maestro-pub-618, verified locally, and waiting in review on PR #9.",
        "2026-03-13: PR #9 review feedback reopened the first phase to harden success-state filtering in the tracker bridge.",
        "2026-03-13: The review fix landed in commit 0975d9e45f85346f450ed1a9be7a76859b466c70 and the thread was replied to on PR #9.",
        "2026-03-13: A later review pass reopened the first phase because fallback transition aliases and missing gh preflight still leave gaps in the PR-backed handoff contract.",
        "2026-03-13: Those latest PR #9 gaps were addressed in commit 08a20f7dfb9526e7421a5f095b1c6adec84e52d6 and both review threads were replied to.",
        "2026-03-13: A newer PR #9 review pass reopened the first phase again because review handoff still accepts stale PR heads and cross-repository PR URLs.",
        "2026-03-13: Those repo-and-HEAD validation gaps were addressed in commit 8987b0853d04950bd52669411c085af507fc0ac5 and both new review threads were replied to.",
        "2026-03-13: Another `@codex review` request was posted on PR #9 and is waiting for any further automated feedback.",
        "2026-03-13: Re-polled PR #9 after the fresh review request; there is still no new Codex review for commit 8987b0853d04950bd52669411c085af507fc0ac5, and all currently visible review threads are resolved.",
        "2026-03-13: A later Codex review on commit 8987b0853d04950bd52669411c085af507fc0ac5 identified one remaining writeback-time PR handoff gap, which was fixed in commit 25c141722f059079ea2751ac824dd628f9446321 and pushed to PR #9 before another `@codex review` request.",
        "2026-03-13: Requested `@codex review` again on PR #9 after the branch was still waiting on a review result for commit 25c141722f059079ea2751ac824dd628f9446321; the newest request comment is acknowledged with `eyes`, but no new automated review is visible yet.",
        "2026-03-13: A newer PR #9 review identified that explicit `maestro:needs-attention` exits still fell into the retry path; commit 2678a10ec51a2aa8593c3a1c2f4a5b5acdf1754a closes that gap, replies in the inline thread, resolves the outdated thread, and posts another `@codex review` request.",
        "2026-03-13: Commit 57fe244bed28a0c99dbe3d34f1fa900ed1ee1980 clarifies the normative spec text for completion dispositions, makes the `review_handoff` versus `manual_attention` split explicit, and requests another `@codex review` on PR #9.",
        "2026-03-13: Commit 9b75d75cccfcc2a4123c2745f90f0475460f45a4 closes the latest PR #9 review gaps by making `maestro:needs-attention` issues ineligible for reselection, broadening GitHub remote parsing to credentialed origins, updating runtime/operator docs for the clear-label recovery step, replying in both review threads, resolving both threads, and requesting another `@codex review`.",
        "2026-03-13: Commit f8897dcebfe8937bf863a00c257b03d3ba64250e removes the remaining user-specific absolute path fixtures from tracked tracker-tool tests, verifies that no tracked file still contains user-specific absolute path fixtures, reruns cargo make lint-fix/fmt/test, and requests another `@codex review` on PR #9.",
        "2026-03-14: After a local self-review, commit 60ba31c366e51ba614c779854b9c2cfbcb5f8dfe tightens `manual_attention` recording to successful label writes only, delays the success completion comment until after the `In Review` state write succeeds, adds regression coverage for both cases, reruns cargo make lint-fix/fmt/test, replies to the two latest inline review threads, resolves both threads, and posts a fresh `@codex review` request at https://github.com/helixbox/maestro/pull/9#issuecomment-4059976931.",
        "2026-03-14: Commit 3344fe0a3670a5485537a34ee21bc03284f44475 closes the remaining self-review gaps by requiring an explanatory comment after a successful `maestro:needs-attention` label write, surfacing partial review-handoff writeback as a non-retryable human-attention failure, updating the runtime and tracker-tool specs to match the enforced contract, rerunning cargo make lint-fix/fmt/test, and requesting another `@codex review` at https://github.com/helixbox/maestro/pull/9#issuecomment-4059991849.",
        "2026-03-14: Rechecked PR #9 review state after Codex returned a top-level no-major-issues comment at https://github.com/helixbox/maestro/pull/9#issuecomment-4060008527, replied to the one remaining outdated inline thread with current-head evidence, resolved it, and confirmed that no review threads remain unresolved.",
        "2026-03-14: Merged PR #9 into `main`, then rewrote the top-of-main squash commit to 90e69adf29ea3d2901399d12be58e1990d4b0206 so `main` again ends with a single-line `delivery/1` commit message; PUB-618 is Done in Linear and PUB-611 is Todo for the daemon supervision phase.",
        "2026-03-14: After clearing the stale PUB-618 and repair workspaces, execution moved into the daemon-supervision subplan and opened lane x/maestro-pub-611 at .workspaces/PUB-611 from origin/main.",
        "2026-03-14: PUB-611 is now on PR #10 with local verification complete and `@codex review` requested; the next phase remains blocked until that PR is reviewed and merged.",
        "2026-03-14: The first PR #10 review round was addressed in commit 7c6d3a68e8d1f98d59cc1b9b838efc09d02751aa, both inline review threads were replied to and resolved, and another `@codex review` request was posted at https://github.com/helixbox/maestro/pull/10#issuecomment-4060190690.",
        "2026-03-14: PR #10 merged into `main`, `origin/main` now points at 7c6d3a68e8d1f98d59cc1b9b838efc09d02751aa with delivery/1 commit messages intact, and PUB-611 is Done in Linear; the next executable phase remains the daemon-supervision subplan at PUB-613.",
        "2026-03-14: Implemented PUB-613 on branch x/maestro-pub-613, verified with cargo make fmt-check/lint/test and live `maestro status` text plus JSON output, opened PR #11 at https://github.com/helixbox/maestro/pull/11, moved PUB-613 to In Review in Linear, and requested `@codex review` after self-review.",
        "2026-03-14: Addressed the first PR #11 review round in commit 3edb7b3c29a61eec49fa75492279379e939b911d by decoupling uncapped active-run status from limit-bound recent runs, rerunning cargo make fmt-check/lint/test, replying in the inline review thread, and resolving the last unresolved thread on PR #11.",
        "2026-03-14: PR #11 was merged into `main` by fast-forward push, preserving the reviewed `delivery/1` commit messages; Linear issue PUB-613 is now Done and the daemon-supervision phase is unblocked.",
        "2026-03-14: Opened PUB-622 as the operator-side pilot tracker and PUB-623 as the fresh bounded Todo issue that should become the first daemon-selected live lane.",
        "2026-03-14: Created a clean runner checkout at `tmp/maestro-runner`, confirmed `cargo run -- protocol probe`, and verified that dry-run selection now targets PUB-623 from the runner-local config.",
        "2026-03-14: Revalidated the umbrella plan after PUB-611 and PUB-613 merged; the only remaining active phase is daemon supervision, with structural follow-ups still intentionally pending behind it.",
        "2026-03-14: The first daemon pilot on PUB-623 reached the live runtime but blocked on PUB-625 after false `stalled_run_detected` failures reported impossible elapsed durations, automatic retries continued after `needs attention` failure comments, and the interrupted daemon left a stale active attempt in runner-local status.",
        "2026-03-14: Implemented PUB-625 on branch x/maestro-pub-625 in commit e95b47249f877762a7f17285c8d0d35a08b1e249, verified with cargo make lint-fix/fmt/lint/test, opened PR #12 at https://github.com/helixbox/maestro/pull/12, moved PUB-625 to In Review, and requested `@codex review` after a focused self-review.",
        "2026-03-14: PR #12 merged by fast-forward push; `origin/main` now points at e95b47249f877762a7f17285c8d0d35a08b1e249 with the reviewed `delivery/1` commit message preserved, and daemon supervision can resume.",
        "2026-03-14: Replaced the stale failed seed lane with fresh issue PUB-626, refreshed `tmp/maestro-runner` to origin/main, reran `cargo run -- protocol probe` plus `cargo run -- run --once --dry-run --config ./tmp/maestro.toml`, and confirmed the daemon pilot now targets x/maestro-pub-626.",
        "2026-03-14: Started the rerun daemon pilot on PUB-626; the issue is now In Progress with a start comment, `maestro status --json` reports active run `pub-626-attempt-1-1773496583`, and the runner workspace contains the expected README plus pilot-guide edits while the live lane is still running.",
        "2026-03-16: Verified the current repo routing is `y/hackink`; the PUB-era helixbox evidence above remains historical provenance from the pre-fork lane, not the current source of execution authority.",
        "2026-03-16: The carried-forward hardening backlog in hackink now maps the completed early phases to XY-134 Done (historical PUB-618), XY-127 Done (historical PUB-611), XY-129 Done (historical PUB-613), and XY-139 Done (historical PUB-625).",
        "2026-03-16: The only still-active phase remains daemon supervision because XY-136 is still In Progress, XY-137 and XY-140 still carry `maestro:needs-attention` follow-up state, and structural follow-ups stay intentionally pending behind that cleanup."
      ],
      "last_updated": "2026-03-16T08:21:28Z",
    "replan_reason": null,
    "context_snapshot": {
      "first_subplan": "docs/plans/2026-03-13_maestro-pr-backed-handoff.md",
      "program_shape": "Three subplans in strict order",
      "active_subplan": "docs/plans/2026-03-13_maestro-daemon-supervision.md",
      "blocking_review_pr": null,
      "blocking_followup_issue": "XY-136",
      "next_validation_issue": "XY-136",
      "active_lane": null,
      "active_workspace": null,
      "tracker_project": "hackink/Maestro Pilot Ops Hardening",
      "imported_issue_map": {
        "pr_handoff": "XY-134",
        "tick_reconciliation": "XY-127",
        "operator_status": "XY-129",
        "pilot": "XY-136",
        "stalled_run_followup": "XY-139"
      },
      "open_followups": [
        "XY-137",
        "XY-140",
        "XY-141"
      ],
      "next_structural_issue_after_daemon_closeout": "XY-125"
    }
  }
}
```

# Split Plan Index

This file is the coordination plan for the split self-bootstrap program. Execution authority for each phase lives in the referenced subplan file.
