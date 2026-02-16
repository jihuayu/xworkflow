use std::collections::HashMap;

use xworkflow::dsl::{parse_dsl, DslFormat};
use xworkflow::{ExecutionStatus, WorkflowHandle, WorkflowRunner};

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

    // Parse
    let schema = parse_dsl(yaml, DslFormat::Yaml).expect("Failed to parse DSL");
    println!(
        "[OK] DSL parsed ({} nodes, {} edges)",
        schema.nodes.len(),
        schema.edges.len()
    );

    let mut inputs = HashMap::new();
    inputs.insert(
        "query".to_string(),
        serde_json::Value::String("Hello, Dify!".to_string()),
    );

    let mut sys = HashMap::new();
    sys.insert(
        "query".to_string(),
        serde_json::Value::String("Hello, Dify!".to_string()),
    );

    let handle: WorkflowHandle = WorkflowRunner::builder(schema)
        .user_inputs(inputs)
        .system_vars(sys)
        .run()
        .await
        .expect("Failed to run workflow");

    match handle.wait().await {
        ExecutionStatus::Completed(outputs) => {
            println!("\n=== Workflow completed ===");
            for (k, v) in &outputs {
                println!("  {} = {}", k, v);
            }
        }
        ExecutionStatus::Failed(error) => {
            println!("\n=== Workflow failed: {} ===", error);
        }
        ExecutionStatus::FailedWithRecovery {
            original_error,
            recovered_outputs,
        } => {
            println!(
                "\n=== Workflow failed with recovery: {} ===",
                original_error
            );
            for (k, v) in &recovered_outputs {
                println!("  {} = {}", k, v);
            }
        }
        ExecutionStatus::Running => {
            println!("\n=== Workflow still running ===");
        }
        ExecutionStatus::Paused {
            node_id, prompt, ..
        } => {
            println!("\n=== Workflow paused at {} ===", node_id);
            println!("{}", prompt);
        }
        ExecutionStatus::WaitingForInput {
            node_id,
            resume_token,
            prompt_text,
            ..
        } => {
            println!("\n=== Workflow waiting for human input at {} ===", node_id);
            println!("resume_token={}", resume_token);
            if let Some(prompt) = prompt_text {
                println!("{}", prompt);
            }
        }
        ExecutionStatus::SafeStopped {
            checkpoint_saved, ..
        } => {
            println!(
                "\n=== Workflow safe-stopped (checkpoint_saved={}) ===",
                checkpoint_saved
            );
        }
    }
}
