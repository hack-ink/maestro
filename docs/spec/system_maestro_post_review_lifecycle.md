# Maestro Post-In-Review Lifecycle

Purpose: Define the normative lifecycle for a Maestro-owned lane after a PR-backed `In Review` handoff, through review follow-up, landing, delivery closeout, and cleanup.
Status: normative
Read this when: You need the authoritative post-`In Review` state model, transition rules, retry/manual-intervention boundaries, or follow-on implementation split for autonomous review follow-up and landing.
Not this document: The low-level app-server protocol, the pre-review runtime handoff contract, the broader owned-lane fallback matrix, or local skill instructions.
Defines: Post-`In Review` lane phases, phase-to-action-class mapping, authoritative signals, retry and cancellation rules, ownership boundaries, and the minimum follow-on implementation split.

## Design reference

- `openai/symphony` `README.md` and `SPEC.md` are the only external design standard for this lifecycle.
- This lifecycle keeps Symphony's stable boundaries intact:
  - the service is a scheduler and runner, not a general-purpose workflow engine
  - workflow policy stays repository-owned
  - a successful worker run may end at a workflow-defined handoff state rather than `Done`
  - operator-visible observability and explicit trust posture remain runtime concerns
- `AGENTS.md` and the checked-in workflow skills are local execution constraints and adapter surfaces. They are not the core domain model of this lifecycle.

## Relationship to other specs

- [`system_maestro_runtime.md`](./system_maestro_runtime.md) defines the current success and failure writeback boundary through PR-backed `In Review` handoff.
- [`system_maestro_owned_lane_policy.md`](./system_maestro_owned_lane_policy.md) defines the allowed action classes and the fallback policy for waiting, repair re-entry, landing readiness, automatic recovery, and manual intervention.
- This document narrows those action classes into the specific post-`In Review` lane phases and transitions that Maestro must honor after review handoff succeeds.

## Core invariants

1. Post-`In Review` work remains part of the same Maestro-owned lane.
2. Post-`In Review` automation must keep using authoritative signals rather than chat memory, branch-name heuristics, or skill-name-specific states.
3. The tracker issue must remain in `In Review` until authoritative delivery closeout transitions it to a terminal completed state, unless a human explicitly cancels or redirects the lane.
4. A later review-repair attempt must resume the retained lane for the same issue and PR head lineage; it must not silently open a fresh unrelated implementation lane.
5. Landing, delivery closeout, and cleanup are deterministic tail stages of the same owned lane, not separate human-only ceremonies.
6. No phase in this lifecycle may depend on the permanent existence of a particular local helper name such as `review-request`, `review-repair`, `pr-land`, or `delivery-closeout`.

## Authoritative signals

Post-`In Review` classification may use only these signal groups:

- Tracker state:
  - issue workflow state
  - labels
  - blocker state
  - authoritative comments written during handoff, repair, landing, or closeout
- Retained-lane state:
  - workspace existence
  - lane markers such as activity and guarded markers
  - current branch, validated head, and retry/churn bookkeeping
- Review state:
  - PR identity and current head
  - review approval or requested-change state
  - unresolved review-thread state
  - required-check state
  - mergeability
- Delivery state:
  - whether merge already happened
  - whether delivery closeout already ran
  - whether workspace and branch cleanup remain pending

If these signals disagree and the disagreement cannot be resolved without guessing operator intent, the runtime must use `manual_intervention_required`.

## Phase model

The post-`In Review` lifecycle is expressed in lane phases. These phases refine, but do not replace, the owned-lane action classes.

| Lane phase | Required action class | Entry conditions | Exit conditions |
| --- | --- | --- | --- |
| `review_wait` | `wait_for_external_signal` | PR-backed `In Review` handoff succeeded for the current owned lane | Actionable review repair appears, landing becomes ready, human intervention becomes required, or cancellation is explicit |
| `review_repair` | `resume_retained_lane` | Actionable review feedback exists and the retained lane still belongs to the same issue and PR lineage | A new repaired head is pushed and review is re-requested for that head, human intervention becomes required, or cancellation is explicit |
| `ready_to_land` | `ready_to_land` | Required approvals are satisfied, blocking review work is absent, checks are green, and the PR is mergeable | Landing begins, signals fall back to wait or repair, or human intervention becomes required |
| `landing` | `continue` | The runtime has committed to executing the merge for the current lane | Merge is recorded, landing fails into a resumable deterministic tail step, or human intervention becomes required |
| `delivery_closeout` | `continue` | Merge already happened for the lane's authoritative anchor and tracker closeout has not yet completed | Tracker closeout succeeds, the lane blocks on contradictory closeout state, or cancellation is explicit |
| `cleanup` | `continue` | Merge and delivery closeout are authoritative and only workspace or branch cleanup remains | The retained workspace and lane branch state are clean, or cleanup blocks on conflicting local evidence |

`manual_intervention_required` is not a normal progress phase. It is the mandatory stop outcome whenever the owned-lane policy says automation must stop.

## Phase semantics

### `review_wait`

This is the default healthy state immediately after PR-backed review handoff.

While in `review_wait`:

- the tracker issue remains in `In Review`
- the retained lane remains reserved for the same issue and PR lineage
- missing immediate review activity is not, by itself, a failure
- review-request acknowledgement probing or bounded resend may happen as orchestration behavior without leaving `review_wait`

`review_wait` must not trigger code changes on its own.

### `review_repair`

`review_repair` means the runtime has enough authoritative evidence to re-enter the retained lane and address review feedback.

While in `review_repair`:

- the runtime must reuse the retained lane when it is still valid
- repair work must stay bound to the same issue, branch lineage, and PR
- the repaired head must pass the local pre-review gate before being pushed
- once a new head is pushed, the lane returns to `review_wait` for that new head

If the retained lane is missing or no longer provably belongs to the same issue and PR lineage, the runtime must not invent a fresh lane silently. It must require human intervention or a separately defined recovery path.

### `ready_to_land`

`ready_to_land` is a decision boundary, not a merge event.

The runtime may classify the lane as `ready_to_land` only when:

- the PR still belongs to the owned lane
- required approvals are satisfied
- actionable blocking review work is absent
- required checks are green
- mergeability is affirmative under repository policy

If any of those signals becomes false again before landing starts, the lane must return to `review_wait` or `review_repair` instead of forcing a merge.

### `landing`

`landing` begins only after `ready_to_land` was true and the runtime committed to the merge step.

While in `landing`:

- the runtime executes the repo-approved merge path
- merge policy is derived from repository policy, not from ad hoc operator chat
- if the PR history is delivery-style, landing must preserve that history shape

If merge succeeds, the lane progresses to `delivery_closeout`. If merge does not succeed and the cause is not self-healing, the runtime must require human intervention rather than guessing whether to retry.

### `delivery_closeout`

`delivery_closeout` begins after merge is authoritative and the tracker issue still needs the final completed-state transition and mirror comment.

While in `delivery_closeout`:

- the merge anchor is authoritative
- tracker closeout runs before GitHub mirror updates
- the tracker issue transitions from `In Review` to the completed state

If merge is authoritative but closeout fails due to a deterministic infrastructure problem with no contradictory state, the runtime may resume `delivery_closeout` later within the same owned lane. If state is contradictory, the runtime must stop for human intervention.

### `cleanup`

`cleanup` is the final deterministic tail stage. It removes retained workspace and lane branch state only after merge and closeout are already authoritative.

`cleanup` must not begin while:

- review work is still pending
- merge is not yet authoritative
- delivery closeout is incomplete

## Transition rules

1. `review_handoff` success in the runtime spec enters `review_wait`.
2. `review_wait -> review_repair` when authoritative review feedback requires a code change and the retained lane is still reusable.
3. `review_repair -> review_wait` after a repaired head is pushed and review is requested for that exact head.
4. `review_wait -> ready_to_land` when approvals, checks, and mergeability all satisfy repository policy for the current lane head.
5. `ready_to_land -> landing` when the runtime begins the merge step.
6. `landing -> delivery_closeout` when merge is authoritative for the lane's anchor.
7. `delivery_closeout -> cleanup` when the tracker closeout succeeds and only deterministic local cleanup remains.
8. `cleanup -> finished` when the retained workspace and lane branch state are clean.

At any phase, contradictory signals or exhausted repair/convergence budgets force `manual_intervention_required`.

## Failure, retry, and cancellation rules

### Review-request lag

- A missing immediate review response is not a failure by itself.
- The runtime may perform bounded resend for the same verified head if the implementation-defined acknowledgement window expires without reliable review-request evidence.
- Exhausting bounded resend without reliable request evidence forces `manual_intervention_required`.

### Review-repair failures

- Transport or runtime interruptions during `review_repair` may resume the same retained lane when the owned-lane policy still permits `resume_retained_lane`.
- Structural churn is not a generic retry case. If repair rounds exceed the configured convergence budget, the runtime must stop for human intervention or architecture rethink rather than patching indefinitely.
- A repair batch that changes the head must return to `review_wait` for that new head instead of continuing downstream on stale review state.

### Landing and closeout failures

- `landing`, `delivery_closeout`, and `cleanup` are deterministic tail stages.
- If their authoritative preconditions are still satisfied, the runtime may resume the same stage later without reopening implementation.
- If merge, tracker state, or workspace ownership becomes contradictory, the runtime must stop for human intervention.

### Cancellation

Cancellation is not a separate owned-lane action class. It is an authoritative external outcome.

Examples:

- the issue is moved to `Canceled` or `Duplicate`
- the PR is closed without merge and the tracker issue is explicitly redirected away from the owned lane

When cancellation is authoritative:

- the runtime must stop autonomous review follow-up and landing
- cleanup may proceed only if the cancellation state is explicit and no contradictory retained-lane evidence remains
- the runtime must not reopen or reinterpret the lane automatically

## Ownership boundaries

### Orchestrator owns

- phase classification for the current lane
- review-request acknowledgement budgets and resend thresholds
- repair convergence budgets
- deciding when a lane is `ready_to_land`
- deciding when contradictory state requires `manual_intervention_required`
- deciding whether a deterministic tail stage may resume automatically

### Local workflow adapters own

- emitting the concrete review-request side effect
- executing review-repair inside the retained lane
- executing the repo-approved land step
- executing tracker closeout and GitHub mirroring
- executing workspace and branch cleanup

### Tracker and GitHub own

- tracker issue workflow state and labels
- PR review state
- required-check state
- mergeability and merge result

The runtime must record the resulting evidence and current phase, but must not elevate current helper names into stable domain states.

## Minimum follow-on implementation split

The accepted post-`In Review` lifecycle maps onto the existing follow-on issues like this:

- `XY-173`: detect owned PR review state and classify `review_wait`, `review_repair`, or `ready_to_land`
- `XY-174`: re-enter retained lanes for `review_repair`
- `XY-175`: implement `landing`, `delivery_closeout`, and `cleanup`
- `XY-177`: align checked-in workflow skills with the accepted lifecycle once the runtime model is stable

These issues remain implementation work. This document is the authoritative lifecycle contract they should implement.
