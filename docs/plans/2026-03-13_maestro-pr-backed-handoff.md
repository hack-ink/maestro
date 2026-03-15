```json
{
  "spec": {
    "schema": "plan/1",
    "plan_id": "maestro-pr-backed-handoff-2026-03-13",
    "goal": "Implement PR-backed review handoff for successful Maestro runs so self-bootstrap lanes become reviewable without manual PR creation.",
    "success_criteria": [
      "PUB-618 is merged to main.",
      "Successful Maestro runs require a pushed branch and linked PR before transitioning the leased issue to In Review.",
      "The runtime, specs, and operator docs all describe In Review as a PR-backed handoff state.",
      "After PUB-618 lands, the project backlog can expose one fresh validation lane without stale review-state ambiguity."
    ],
    "constraints": [
      "Keep WORKFLOW.md on TOML frontmatter.",
      "Keep Linear as the workflow source of truth.",
      "Do not redesign the full GitHub integration surface beyond the bounded PR-backed success path.",
      "Do not widen this plan into daemon stall detection, operator status surface, retry taxonomy, or remove-SQLite work."
    ],
    "defaults": {
      "authority_issue": "PUB-618",
      "candidate_validation_issue": "PUB-611",
      "config_path": "./tmp/maestro.toml",
      "execution_project": "Maestro Pilot Ops Hardening",
      "preflight_commands": [
        "cargo run -- protocol probe",
        "cargo run -- run --once --dry-run --config ./tmp/maestro.toml"
      ],
      "verification_commands": [
        "cargo make fmt-check",
        "cargo make lint",
        "cargo make test"
      ]
    },
    "tasks": [
      {
        "id": "confirm-baseline",
        "title": "Confirm the project baseline is ready for PR-backed handoff work",
        "status": "done",
        "objective": "Anchor the plan to the already-cleaned backlog and repo state so PUB-618 starts from current evidence.",
        "inputs": [
          "Maestro Pilot Ops Hardening project state",
          "Current main branch state",
          "Current runtime and workflow specs"
        ],
        "outputs": [
          "PUB-618 is the current Todo issue",
          "PUB-607, PUB-614, PUB-615, and PUB-619 are already Done",
          "Root checkout is aligned to origin/main except for planning artifacts"
        ],
        "verification": [
          "Read Linear issue states for PUB-618 and the recently closed hardening issues",
          "git status --short --branch"
        ],
        "depends_on": []
      },
      {
        "id": "design-pr-handoff-contract",
        "title": "Define the PR-backed success contract",
        "status": "done",
        "objective": "Turn the current manual PR-assisted review pattern into an explicit runtime contract before code changes begin.",
        "inputs": [
          "PUB-618",
          "docs/spec/system_maestro_runtime.md",
          "WORKFLOW.md",
          "The merged examples from PUB-606, PUB-614, PUB-616, and PUB-617"
        ],
        "outputs": [
          "An explicit success-path contract for push, PR creation, PR linking, and In Review transition order",
          "Clear scope boundaries for what stays manual versus automated in this phase"
        ],
        "verification": [
          "The updated plan scope still excludes broader GitHub lifecycle redesign",
          "The contract order is explicit enough to test"
        ],
        "depends_on": [
          "confirm-baseline"
        ]
      },
      {
        "id": "implement-pub-618",
        "title": "Implement and document PR-backed handoff",
        "status": "done",
        "objective": "Change the runtime so a successful run cannot claim In Review without first producing a reviewable PR surface.",
        "inputs": [
          "design-pr-handoff-contract outputs",
          "Relevant runtime code paths",
          "Current GitHub CLI or API integration surface already used manually"
        ],
        "outputs": [
          "Code changes for push and PR-backed success handling",
          "Updated runtime spec and operator docs",
          "Tests covering the new success-path ordering"
        ],
        "verification": [
          "cargo make fmt-check",
          "cargo make lint",
          "cargo make test"
        ],
        "depends_on": [
          "design-pr-handoff-contract"
        ]
      },
      {
        "id": "prepare-next-validation-lane",
        "title": "Leave one fresh validation lane ready after PUB-618 lands",
        "status": "done",
        "objective": "Ensure the next live self-bootstrap pass can immediately validate the new contract instead of first cleaning tracker state.",
        "inputs": [
          "Merged PUB-618",
          "PUB-611",
          "Current project issue states"
        ],
        "outputs": [
          "Exactly one intended next live lane exposed for validation, preferably PUB-611",
          "No stale In Review issue left to confuse future selection"
        ],
        "verification": [
          "Linear shows PUB-618 closed and one intended next validation lane ready",
          "The next lane is fresh work rather than a historical already-finished issue"
        ],
        "depends_on": [
          "implement-pub-618"
        ]
      }
    ],
    "replan_policy": {
      "owner": "plan-writing",
      "triggers": [
        "PUB-618 requires broader GitHub lifecycle automation than a bounded success-path handoff",
        "The required PR-linked success order cannot be expressed cleanly in the current runtime boundaries",
        "A different fresh validation issue becomes a safer first proof than PUB-611"
      ]
    }
  },
  "state": {
    "phase": "done",
    "current_task_id": null,
    "next_task_id": null,
    "blockers": [],
    "evidence": [
      "2026-03-13: PUB-618 was promoted to Todo as the current next live lane.",
      "2026-03-13: PUB-607, PUB-614, PUB-615, and PUB-619 were cleaned up to Done during backlog reconciliation.",
        "2026-03-13: The current review surface still depends on manual PR creation after a successful run.",
        "2026-03-13: Design conclusion for PUB-618: keep GitHub authoring in the agent lane via existing git and gh commands, add a dedicated success-path tool that validates a PR-backed review handoff, and stop allowing generic issue_transition directly into the success state.",
        "2026-03-13: Implemented PUB-618 on branch x/maestro-pub-618, verified with cargo make fmt-check, cargo make lint, and cargo make test, and opened PR #9.",
        "2026-03-13: Linear issue PUB-618 is now In Review with PR https://github.com/helixbox/maestro/pull/9.",
        "2026-03-13: PR #9 review feedback identified that issue_transition can still reach the success state if startable_states overlaps with success_state, so implement-pub-618 was reopened.",
        "2026-03-13: Addressed the overlap bug in commit 0975d9e45f85346f450ed1a9be7a76859b466c70, replied in the review thread, and reran cargo make fmt-check, cargo make lint, and cargo make test.",
        "2026-03-13: A follow-up PR #9 review pass identified two additional gaps: fallback transition states can still alias the success state, and live runs do not yet preflight the gh dependency required for service-side review handoff validation.",
        "2026-03-13: Addressed the latest PR #9 review feedback in commit 08a20f7dfb9526e7421a5f095b1c6adec84e52d6, replied in both review threads, and reran cargo make fmt-check, cargo make lint, and cargo make test.",
        "2026-03-13: Another PR #9 review pass identified that review handoff still does not verify PR repository identity or that the PR head commit matches the validated lane HEAD.",
        "2026-03-13: Addressed the repo-and-HEAD validation gaps in commit 8987b0853d04950bd52669411c085af507fc0ac5, replied in both new review threads, and reran cargo make fmt-check, cargo make lint, and cargo make test.",
        "2026-03-13: Posted another `@codex review` request on PR #9 after commit 8987b0853d04950bd52669411c085af507fc0ac5 and polled for follow-up feedback; the request is acknowledged but no new inline comments have arrived yet.",
        "2026-03-13: Re-polled PR #9 review records and review threads after the fresh request; there is still no new Codex review for commit 8987b0853d04950bd52669411c085af507fc0ac5, and every currently visible review thread is resolved.",
        "2026-03-13: A later Codex review on commit 8987b0853d04950bd52669411c085af507fc0ac5 identified one remaining gap: `apply_review_handoff` must revalidate PR/local HEAD state immediately before writeback instead of trusting only the earlier cached handoff.",
        "2026-03-13: Addressed the writeback-time revalidation gap in commit 25c141722f059079ea2751ac824dd628f9446321, replied in the inline review thread, reran cargo make lint-fix, cargo make fmt, and cargo make test, pushed the branch, and requested another `@codex review` on PR #9.",
        "2026-03-13: Requested `@codex review` again on PR #9 after the branch was still waiting on a review result for commit 25c141722f059079ea2751ac824dd628f9446321; the newest request comment is acknowledged with `eyes`, but no new automated review is visible yet.",
        "2026-03-13: A newer Codex review identified that the success path still retried lanes that explicitly requested `maestro:needs-attention`; commit 2678a10ec51a2aa8593c3a1c2f4a5b5acdf1754a records manual-attention exits as a distinct completion disposition, routes them through immediate human-required failure handling, updates the runtime/operator docs, replies in the PR thread, resolves the outdated thread, and requests another `@codex review`.",
        "2026-03-13: Commit 57fe244bed28a0c99dbe3d34f1fa900ed1ee1980 clarifies the normative spec text for completion dispositions, explicitly documents the mutually exclusive `review_handoff` versus `manual_attention` signals, and requests another `@codex review` on PR #9.",
        "2026-03-13: Commit 9b75d75cccfcc2a4123c2745f90f0475460f45a4 closes the latest PR #9 review gaps by making `maestro:needs-attention` issues ineligible for reselection, broadening GitHub remote parsing to credentialed origins, updating runtime/operator docs for the clear-label recovery step, replying in both review threads, resolving both threads, and requesting another `@codex review`.",
        "2026-03-13: Commit f8897dcebfe8937bf863a00c257b03d3ba64250e removes the remaining user-specific absolute path fixtures from tracked tracker-tool tests, verifies that no tracked file still contains user-specific absolute path fixtures, reruns cargo make lint-fix/fmt/test, and requests another `@codex review` on PR #9.",
        "2026-03-14: After a local self-review, commit 60ba31c366e51ba614c779854b9c2cfbcb5f8dfe tightens `manual_attention` recording to successful label writes only, delays the success completion comment until after the `In Review` state write succeeds, adds regression coverage for both cases, reruns cargo make lint-fix/fmt/test, replies to the two latest inline review threads, resolves both threads, and posts a fresh `@codex review` request at https://github.com/helixbox/maestro/pull/9#issuecomment-4059976931.",
        "2026-03-14: Commit 3344fe0a3670a5485537a34ee21bc03284f44475 closes the remaining self-review gaps by requiring an explanatory comment after a successful `maestro:needs-attention` label write, surfacing partial review-handoff writeback as a non-retryable human-attention failure, updating the runtime and tracker-tool specs to match the enforced contract, rerunning cargo make lint-fix/fmt/test, and requesting another `@codex review` at https://github.com/helixbox/maestro/pull/9#issuecomment-4059991849.",
        "2026-03-14: Rechecked PR #9 review state after Codex returned a top-level no-major-issues comment at https://github.com/helixbox/maestro/pull/9#issuecomment-4060008527, replied to the one remaining outdated inline thread with current-head evidence, resolved it, and confirmed that no review threads remain unresolved.",
        "2026-03-14: Merged PR #9 into `main`, then rewrote the top-of-main squash commit to 90e69adf29ea3d2901399d12be58e1990d4b0206 so the branch head again uses a single-line `delivery/1` commit message; PUB-618 is Done in Linear and PUB-611 is the fresh next validation lane."
      ],
      "last_updated": "2026-03-14T09:11:30Z",
    "replan_reason": null,
    "context_snapshot": {
      "current_gap": "PUB-618 is merged and closed; the next execution entry is the fresh validation lane PUB-611.",
      "next_issue_after_merge": "PUB-611",
      "active_branch": "main",
      "active_commit": "90e69adf29ea3d2901399d12be58e1990d4b0206",
      "active_pr_url": "https://github.com/helixbox/maestro/pull/9",
      "pub_618_contract": {
        "agent_owned_steps": [
          "commit local changes",
          "push the lane branch",
          "create or update the pull request"
        ],
        "runtime_owned_steps": [
          "expose a dedicated success-path review handoff tool",
          "verify the supplied PR reference against the current lane",
          "write the completion comment and only then transition the issue to In Review"
        ],
        "forbidden_shortcut": "Generic issue_transition must no longer move the leased issue directly into the success state."
      }
    }
  }
}
```

# PR-Backed Handoff Plan

This plan isolates the missing review contract from later daemon and observability work. It ends when PR-backed handoff is merged and one fresh validation lane is ready.
