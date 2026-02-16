# Repository Guidelines

## Project Structure & Module Organization

- `src/`: main runtime crate (`xworkflow`) and CLI entry (`src/main.rs`); core modules include `core/`, `dsl/`, `graph/`, `nodes/`, `plugin_system/`, `sandbox/`, `security/`.
- `crates/`: workspace crates (shared types + optional sandboxes/plugins), e.g. `crates/xworkflow-types`, `crates/xworkflow-sandbox-js`, `crates/xworkflow-sandbox-wasm`.
- `tests/`: integration-style runners in `tests/*.rs`, plus fixtures under `tests/integration/cases/`.
- `examples/`: runnable demos (e.g. `examples/scheduler_demo.rs`).
- `benches/`: Criterion benchmarks.
- `docs/`: design documents and implementation notes (CI ignores `docs/**` changes).

## Build, Test, and Development Commands

CI runs tests with `--all-features`, so prefer these locally:

```bash
cargo build --all-features
cargo test --all-features --workspace
cargo test --all-features --workspace --test integration_tests
cargo test --all-features --workspace --test memory_tests -- --test-threads=1
cargo clippy --all-features --workspace
cargo fmt
cargo run --example scheduler_demo
cargo bench --all-features
```

## Coding Style & Naming Conventions

- Rust 2021, `rustfmt` defaults (`cargo fmt`) and `clippy` (`cargo clippy`).
- Follow the project’s priority order: security first, then performance, then “obvious” code (no hidden side effects or surprising defaults).
- Keep feature-gated code symmetric (`#[cfg(feature = "...")]`) and compiling both enabled and disabled.

## Testing Guidelines

- Prefer unit tests near the code they cover; use `tests/*.rs` for cross-crate and end-to-end scenarios.
- Integration fixtures live in `tests/integration/cases/NNN_short_name/` with `workflow.toml`, `in.toml`, `out.toml`, `state.toml`.

## Commit & Pull Request Guidelines

- Commit messages generally follow Conventional Commits: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:` (optional scopes like `feat(security):`).
- PRs should include: what/why, how to test (paste the exact `cargo ...` commands), feature flags touched, and any doc updates required for behavior changes.
- For perf-sensitive changes, include benchmark notes (`cargo bench --all-features`) and call out hot-path allocation changes explicitly.

## Agent-Specific Notes

- `CLAUDE.md` documents repo-specific engineering rules (validation order, feature-flag correctness, and doc-sync expectations). Use it as the source of truth when changing behavior or protocols.
