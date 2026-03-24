# Maestro

Repo-native orchestration for autonomous coding agents.

Maestro is a standalone control plane for issue-driven coding workflows. It is inspired by the OpenAI Symphony model, but implemented as a helixbox-owned runtime that reads repo-local workflow policy, provisions isolated workspaces, and drives Codex through the direct `app-server` protocol.

## Status

The repository is in active bootstrap. The current milestone is the MVP:

- one configured Linear scope
- one eligible issue
- one isolated workspace
- one `codex app-server` run
- one authoritative outcome written back to Linear

The CLI surface exists now as a scaffold. The orchestration baseline is in place, and the current follow-up checkpoint is the split self-bootstrap program index in [`docs/plans/2026-03-13_maestro-first-live-pilot-hardening.md`](docs/plans/2026-03-13_maestro-first-live-pilot-hardening.md), which sequences the PR-backed handoff phase before daemon supervision and structural follow-ups.

## Current CLI Shape

```sh
cargo run -- protocol probe
cargo run -- status --config ./tmp/maestro.toml
cargo run -- run --once --dry-run --config ./tmp/maestro.toml
cargo run -- run --once --config ./tmp/maestro.toml
cargo run -- daemon --poll-interval-s 60 --config ./tmp/maestro.toml
```

These commands are intentionally early-stage entrypoints. The `protocol probe` command is the first contract check for `app-server` compatibility before the full orchestrator loop depends on it.

When you need a shorter operator snapshot, pass `--limit` to `cargo run -- status --config ./tmp/maestro.toml`. That limit only truncates the `Recent Runs` section; `Active Runs` remain fully visible so the currently leased lanes never disappear from the status view.

The status snapshot now carries operator-facing liveness fields in both text and JSON output. In addition to `run_id`, `issue_id`, `thread_id`, `branch`, and `workspace_path`, each run reports a coarse `phase`, optional `wait_reason`, the last observed run and protocol activity timestamps, idle seconds, and any queued retry schedule that is still holding the lane.

If you need remote read-only inspection while daemon mode is running, configure `[operator_http]` in `maestro.toml`. The listener is disabled by default; when enabled it serves the same JSON status snapshot model from `GET /state`.

## Pilot Guide

For the first real pilot, target `maestro` itself before onboarding another repository. Keep the live service config at `./tmp/maestro.toml`, and keep issue workspaces under the repo-local `.workspaces/` directory. If you need a tracked template, start from `./maestro.example.toml`. Each lane is now a clone-backed workspace that keeps its own `.git` metadata inside the lane instead of relying on shared Git administrative storage. Configure secret authority through TOML by naming the required environment variables in `[tracker].api_key_env_var` and `[github].token_env_var`; `maestro` does not infer GitHub auth from ambient shell state.

Recommended order:

```sh
cargo run -- protocol probe
cargo run -- status --config ./tmp/maestro.toml
cargo run -- run --once --dry-run --config ./tmp/maestro.toml
cargo run -- run --once --config ./tmp/maestro.toml
```

After those bounded checks pass, switch to `cargo run -- daemon --poll-interval-s 60 --config ./tmp/maestro.toml` when you want the long-running poll loop for the pilot. Daemon mode currently requires a Unix target because the parent process hands the single project dispatch-slot lock to the spawned `run --once` child via file-descriptor inheritance.

The detailed operator runbook, including sample config, filesystem layout, and failure inspection, lives in [`docs/guide/pilot.md`](docs/guide/pilot.md).

## Documentation

- Repository overview: [`docs/index.md`](docs/index.md)
- Specifications: [`docs/spec/index.md`](docs/spec/index.md)
- Operational guides: [`docs/guide/index.md`](docs/guide/index.md)
- Repository workflow contract: [`WORKFLOW.md`](WORKFLOW.md)
- Pilot runbook: [`docs/guide/pilot.md`](docs/guide/pilot.md)
- Current implementation plan index (split self-bootstrap program): [`docs/plans/2026-03-13_maestro-first-live-pilot-hardening.md`](docs/plans/2026-03-13_maestro-first-live-pilot-hardening.md)

## Development

Build and verify from the repository root:

```sh
cargo run -- --help
cargo make fmt-check
cargo make lint
cargo make test
```

`cargo make` is the source of truth for repo-native verification and formatting entrypoints.

## License

Licensed under [GPL-3.0](LICENSE).
