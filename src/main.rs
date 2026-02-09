use tokio::sync::mpsc;

use xworkflow::core::dispatcher::{EngineConfig, WorkflowDispatcher};
use xworkflow::core::variable_pool::{Segment, VariablePool};
use xworkflow::dsl::{parse_dsl, DslFormat};
use xworkflow::dsl::validator::validate_workflow_schema;
use xworkflow::graph::build_graph;
use xworkflow::nodes::executor::NodeExecutorRegistry;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    println!("=== XWorkflow Engine (Dify-compatible) ===\n");

    let yaml = r#"
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
  - id: if1
    data:
      type: if-else
      title: Check Length
      cases:
        - case_id: long
          logical_operator: and
          conditions:
            - variable_selector: ["start", "query"]
              comparison_operator: not_empty
              value: null
  - id: answer_yes
    data:
      type: answer
      title: Has Query
      answer: "You said: {{#start.query#}}"
  - id: answer_no
    data:
      type: answer
      title: No Query
      answer: "No query provided."
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["answer_yes", "answer"]
edges:
  - source: start
    target: if1
  - source: if1
    target: answer_yes
    sourceHandle: long
  - source: if1
    target: answer_no
    sourceHandle: "false"
  - source: answer_yes
    target: end
  - source: answer_no
    target: end
"#;

    // Parse and validate
    let schema = parse_dsl(yaml, DslFormat::Yaml).expect("Failed to parse DSL");
    validate_workflow_schema(&schema).expect("Schema validation failed");
    println!("[OK] DSL parsed and validated ({} nodes, {} edges)", schema.nodes.len(), schema.edges.len());

    // Build graph
    let graph = build_graph(&schema).expect("Failed to build graph");
    println!("[OK] Graph built (root: {})", graph.root_node_id);

    // Initialize variable pool
    let mut pool = VariablePool::new();
    pool.set(
        &["start".to_string(), "query".to_string()],
        Segment::String("Hello, Dify!".to_string()),
    );
    pool.set(
        &["sys".to_string(), "query".to_string()],
        Segment::String("Hello, Dify!".to_string()),
    );

    // Run
    let registry = NodeExecutorRegistry::new();
    let (tx, mut rx) = mpsc::channel(100);
    let config = EngineConfig::default();

    let mut dispatcher = WorkflowDispatcher::new(graph, pool, registry, tx, config);

    let handle = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let json = event.to_json();
            let event_type = json["type"].as_str().unwrap_or("unknown");
            match event_type {
                "graph_run_started" => println!("\n>>> Graph execution started"),
                "graph_run_succeeded" => {
                    println!(">>> Graph execution succeeded");
                    if let Some(outputs) = json["data"]["outputs"].as_object() {
                        for (k, v) in outputs {
                            println!("    Output: {} = {}", k, v);
                        }
                    }
                }
                "graph_run_failed" => {
                    println!(">>> Graph execution FAILED: {}", json["data"]["error"]);
                }
                "node_run_started" => {
                    println!(
                        "  > Node started: {} ({})",
                        json["data"]["node_title"].as_str().unwrap_or(""),
                        json["data"]["node_type"].as_str().unwrap_or("")
                    );
                }
                "node_run_succeeded" => {
                    println!(
                        "  > Node succeeded: {}",
                        json["data"]["node_id"].as_str().unwrap_or("")
                    );
                }
                _ => {
                    println!("  > Event: {}", event_type);
                }
            }
        }
    });

    let result = dispatcher.run().await;
    drop(dispatcher);

    // Wait for event handler to finish
    let _ = handle.await;

    match result {
        Ok(outputs) => {
            println!("\n=== Workflow completed ===");
            for (k, v) in &outputs {
                println!("  {} = {}", k, v);
            }
        }
        Err(e) => {
            println!("\n=== Workflow failed: {} ===", e);
        }
    }
}
