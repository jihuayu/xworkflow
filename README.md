# xworkflow: Universal Workflow Runtime (Rust)

> **⚠️ EARLY DEVELOPMENT WARNING**
> 
> This project is in very early stages of active development. APIs are unstable and subject to breaking changes without notice. Not recommended for production use yet.

xworkflow is a universal workflow runtime designed for "embeddable" scenarios: it parses and validates a workflow DSL, then executes it in-memory as a directed graph. Its goal is not to be a "platform," but rather a runtime that can be integrated into host processes written in Node.js, Python, Go, Rust, and other languages.

This project is designed for **short-lived workflows that do not require mid-execution persistence**, emphasizing three key principles:

- **Fast**: Tokio async runtime with concurrent scheduling, minimizing unnecessary synchronization and copying.
- **Safe**: Rust memory safety; optional sandbox and security capabilities (via feature flags).
- **Explicit**: APIs and behaviors are intentionally explicit — no reliance on implicit global state, no hidden storage writes, no concealed side effects; all I/O and extension points are expressed through clear node/config/plugin interfaces.

## Key Capabilities

- **DSL Parsing & Validation**: Supports YAML and JSON formats with schema validation and diagnostic reporting.
- **Graph Execution Model**: Execution based on nodes and edges; runtime data managed through `VariablePool`.
- **Values + Streams**: Variable segments (Segment) support both plain values and streams, enabling nodes with streaming output.
- **Pluggable Node System**: Built-in common nodes; also supports loading plugins via dynamic libraries (optional feature).
- **Sandboxed Execution**: Built-in JavaScript (Boa) and WASM (Wasmtime) runtimes for isolated execution of untrusted code (see implementation and features for details).

## Quick Start

### Prerequisites

- Rust stable (recent version recommended)

### Build & Test

```bash
cargo build
cargo test
```

### Run Example

The repository includes a runnable example:

```bash
cargo run --example scheduler_demo
```

### Minimal Rust Usage Example

Below is a minimal example showing how to parse a YAML DSL and execute a simple workflow (Start → End):

```rust
use serde_json::json;
use std::collections::HashMap;

use xworkflow::dsl::{parse_dsl, DslFormat};
use xworkflow::scheduler::{ExecutionStatus, WorkflowRunner};

#[tokio::main]
async fn main() {
    let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          label: Query
          type: string
          required: true
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["start", "query"]
edges:
  - source: start
    target: end
"#;

    let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();

    let mut inputs = HashMap::new();
    inputs.insert("query".into(), json!("hello"));

    let handle = WorkflowRunner::builder(schema)
        .user_inputs(inputs)
        .run()
        .await
        .unwrap();

    match handle.wait().await {
        ExecutionStatus::Completed(outputs) => println!("outputs={:?}", outputs),
        other => println!("status={:?}", other),
    }
}
```

## Embedding in Other Languages

The core form of this project is a Rust crate (used directly as a dependency in Rust host applications).

**Multi-Language Bindings (Planned)**: Future plans include support for embedding in Node.js, Python, Go, and other host processes. Recommended implementation approaches:

- Expose runtime entry points via **C ABI / dynamic library**, with the host calling through FFI;
- Or provide more "explicit" binding layers for target languages (e.g., Node-API, Python extensions, cgo wrappers).

The repository already includes **dynamic library plugin** related ABI and sample node plugins (see `crates/*-dylib`), which load node capabilities as shared libraries on-demand. These mechanisms can serve as the foundation for multi-language bindings.

## Directory Structure

```
.
├── src/
│   ├── core/           # Runtime core: scheduler, variable pool, context, etc.
│   ├── dsl/            # DSL parsing & validation
│   ├── evaluator/      # Condition/expression evaluation
│   ├── graph/          # Graph construction & execution
│   ├── llm/            # LLM-related capabilities (depending on nodes/implementation)
│   ├── nodes/          # Built-in node executors
│   ├── plugin/         # Plugin foundations
│   ├── plugin_system/  # Plugin system (optional feature)
│   ├── sandbox/        # JS/WASM sandboxes
│   ├── security/       # Security capabilities (optional feature)
│   ├── template/       # Template rendering
│   └── scheduler.rs    # Workflow scheduler & handle API
├── crates/             # Reusable types & various node plugins (including dynamic library versions)
├── examples/           # Runnable examples
├── benches/            # Benchmarks (criterion)
└── docs/               # Design documents & specifications
```

## Documentation

For detailed design documents and implementation status, see:

- **[设计文档索引](docs/设计文档索引.md)** - Quick reference for all design documents with implementation status
- **[实现状态总结](docs/实现状态总结.md)** - Comprehensive analysis of implemented features

The [docs/](docs/) directory contains 45+ design documents covering:
- Core workflow engine and node execution specifications
- Plugin system and sandbox architectures
- Security and resource governance
- Testing frameworks and benchmarks
- Performance optimization strategies
