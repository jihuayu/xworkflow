# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Design Goals

This crate's design priorities, in order:

1. **Security** — Defense in depth: input validation, sandbox hardening, network policy, resource governance, audit. Untrusted code runs in sandboxes. Secrets never leak into logs or errors.
2. **High Performance** — Zero-cost abstractions, compile-time feature elimination, minimal allocations. Hot paths must be allocation-free or use copy-on-write. No unnecessary indirection.
3. **Obviousness** — Code must be concise, predictable, and free of hidden traps. No implicit behavior, no magic, no surprising defaults. Every API should do exactly what its name says — nothing more, nothing less. When reading any function, the reader should be able to understand its full behavior without chasing through layers of indirection.

All contributions and refactors must uphold these three principles. When in conflict, resolve in the listed priority order.

## Project Overview

xworkflow is a Rust-based universal workflow runtime for embeddable scenarios. It parses a workflow DSL (YAML/JSON), validates it, builds a DAG, and executes it in-memory with Tokio. Designed for short-lived workflows without mid-execution persistence.

## Build & Test Commands

```bash
# Build
cargo build --all-features

# Run all tests
cargo test --all-features --workspace

# Run specific test categories
cargo test --all-features --workspace --lib --bins          # Unit tests
cargo test --all-features --workspace --test integration_tests
cargo test --all-features --workspace --test plugin_system_registry
cargo test --all-features --workspace --test plugin_system_tests
cargo test --all-features --workspace --test memory_tests -- --test-threads=1

# Run a single test by name
cargo test test_name --all-features -- --nocapture

# Benchmarks
cargo bench --all-features

# Run example
cargo run --example scheduler_demo

# Lint
cargo clippy --all-features --workspace
```

## Design-to-Implementation Update Rules (General)

When a task says “complete design gaps and update docs”, follow this sequence strictly:

1. **Audit first, then code**
  - Compare design doc requirements against current code paths before implementing.
  - Mark each requirement as: implemented / partial / missing.
  - Do not update status docs to ✅ before code + tests prove completeness.

2. **Fix at the protocol boundary, not only local logic**
  - If a feature changes runtime behavior (pause/resume, status transitions, control commands), update all related layers together:
    - DSL schema + validation
    - node executor
    - dispatcher command handling
    - scheduler/workflow handle APIs
    - event bus/status surface
  - Keep backward compatibility where possible (e.g., keep legacy command/API while adding richer protocol).

3. **Feature-flag correctness is required**
  - New capability behind optional features must compile cleanly for both enabled/disabled paths.
  - Use `#[cfg(...)]` symmetrically for data structures, API methods, and runtime branches.

4. **Documentation must match real code paths**
  - After implementation, update at least:
    - design index (`docs/设计文档索引.md`)
    - implementation status summary (`docs/实现状态总结.md`)
    - the specific design doc(s) that describe file locations/steps
  - Ensure file paths and module names in docs match actual code locations.
  - Keep status counts/percentages internally consistent after status flips.

5. **Validation order: targeted → full regression**
  - First run focused tests for changed areas.
  - Then run full regression:
    - `cargo test --all-features -- --nocapture`
  - Then verify compile health and warning cleanliness:
    - `cargo test --all-features --no-run`
    - `cargo check --workspace --all-targets --all-features`

6. **Warnings are treated as debt to clear immediately**
  - Do not leave newly observed warnings unresolved.
  - Prefer minimal, behavior-preserving fixes (remove dead code, tighten cfg scope, fix unused assignments/imports).

7. **Deliverable quality bar**
  - Final state should satisfy all of the following:
    - changed feature behavior is covered by tests,
    - no compiler warnings in full-target workspace check,
    - documentation status reflects actual implementation truth.

## Feature Flags

Default features: `security`, `plugin-system`, and all `builtin-*` node types. Use `--all-features` for full builds. Key optional features:

- `security` — Resource governance, network policy, SSRF prevention, audit logging
- `plugin-system` — Dynamic library loading, two-phase plugin lifecycle
- `wasm-runtime` — WASM plugin runtime via wasmtime
- `builtin-sandbox-js` / `builtin-sandbox-wasm` — Sandboxed code execution
- `builtin-code-node` — Requires `builtin-sandbox-js`

## Architecture

### Execution Flow

```
WorkflowRunner::builder(schema) → parse DSL → validate (3 layers) → build Graph
  → create VariablePool → register NodeExecutors → init plugins → spawn dispatcher
  → DAG traversal (edge resolution → node execution → variable updates → events)
  → WorkflowHandle (async, non-blocking)
```

### Key Modules

- **`api/runner.rs`** — Public API entry point. `WorkflowRunner` builder for executing a parsed workflow schema.
- **`api/handle.rs`** — Public handle type (`WorkflowHandle`) re-exported from `application`.
- **`core/dispatcher.rs`** — DAG execution engine. Resolves edges, executes nodes, handles errors/retries.
- **`core/variable_pool.rs`** — Variable system using copy-on-write (`im::HashMap`). Supports streaming segments for LLM output.
- **`core/runtime_context.rs`** — Injectable context with `TimeProvider` and `IdGenerator` traits for testability.
- **`dsl/`** — Schema types, YAML/JSON parser, three-layer validation (structure, topology, semantics).
- **`graph/`** — `Graph` construction from schema. Adjacency lists, `EdgeTraversalState`.
- **`nodes/executor.rs`** — `NodeExecutor` trait + registry with lazy/eager initialization.
- **`plugin_system/`** — Two-phase lifecycle: Bootstrap (infrastructure: sandboxes, template engines, loaders) then Normal (app: nodes, hooks, providers).
- **`security/`** — Five layers: input validation, sandbox hardening, network security, resource governance, audit.

### Workspace Crates

- `xworkflow-types` — Shared types
- `xworkflow-sandbox-js` — JavaScript sandbox (Boa engine)
- `xworkflow-sandbox-wasm` — WASM sandbox (Wasmtime)
- `xworkflow-plugin-wasm` — WASM plugin runtime
- `xworkflow-nodes-code` / `xworkflow-nodes-code-dylib` — Code node executor variants

### Key Patterns

- **Registry pattern**: `NodeExecutorRegistry` and `LlmProviderRegistry` use `HashMap` with `OnceLock` (eager) or factory closures (lazy).
- **Gate/Hook pattern**: `SecurityGate`, `PluginGate`, `DebugGate` — trait-based pluggable enforcement via `Option<Arc<dyn Gate>>` with no-op defaults.
- **Copy-on-Write variables**: `VariablePool` uses `im::HashMap` for snapshot isolation between nodes and sub-graphs.
- **Feature gating**: `#[cfg(feature = "...")]` eliminates code at compile time. Optional subsystems have zero runtime cost when disabled.
- **Error strategies**: Per-node (`None`, `FailBranch`, `DefaultValue`) with retry/backoff, plus optional global error handler sub-graph.

### Core Traits

- `NodeExecutor` — Async node execution: `execute(node_id, config, variable_pool, context) -> NodeRunResult`
- `Plugin` — Two-phase lifecycle: `register(context)` + `shutdown()`
- `ResourceGovernor` — Quota enforcement per resource group
- `CredentialProvider` — Secret lookup by group/provider
- `SubGraphRunner` — Iteration/loop sub-graph execution
- `TimeProvider` / `IdGenerator` — Injectable for deterministic testing
