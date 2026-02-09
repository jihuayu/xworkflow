use serde_json::json;
use std::collections::HashMap;

use xworkflow::dsl::{parse_dsl, DslFormat};
use xworkflow::scheduler::{ExecutionStatus, WorkflowRunner};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("=== XWorkflow Scheduler Demo ===\n");

    // --- Demo 1: Simple Start → End ---
    println!("--- Demo 1: Simple pipeline ---");
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
    inputs.insert("query".into(), json!("Hello from scheduler!"));

    let mut sys = HashMap::new();
    sys.insert("query".into(), json!("Hello from scheduler!"));

    let handle = WorkflowRunner::builder(schema)
      .user_inputs(inputs)
      .system_vars(sys)
      .run()
      .await
      .unwrap();
    let status = handle.wait().await;
    match status {
      ExecutionStatus::Completed(outputs) => println!("Result: {:?}\n", outputs),
      other => println!("Result: {:?}\n", other),
    }

    // --- Demo 2: Branch workflow ---
    println!("--- Demo 2: IfElse branch ---");
    let yaml2 = r#"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: if1
    data:
      type: if-else
      title: Check
      cases:
        - case_id: big
          logical_operator: and
          conditions:
            - variable_selector: ["start", "n"]
              comparison_operator: greater_than
              value: 5
  - id: end_big
    data:
      type: answer
      title: Big
      answer: "Number {{#start.n#}} is big!"
  - id: end_small
    data:
      type: answer
      title: Small
      answer: "Number {{#start.n#}} is small."
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: answer
          value_selector: ["end_big", "answer"]
edges:
  - source: start
    target: if1
  - source: if1
    target: end_big
    sourceHandle: big
  - source: if1
    target: end_small
    sourceHandle: "false"
  - source: end_big
    target: end
  - source: end_small
    target: end
"#;
    let schema2 = parse_dsl(yaml2, DslFormat::Yaml).unwrap();

    let mut inputs2 = HashMap::new();
    inputs2.insert("n".into(), json!(10));

    let handle2 = WorkflowRunner::builder(schema2)
      .user_inputs(inputs2)
      .run()
      .await
      .unwrap();
    let status2 = handle2.wait().await;
    match status2 {
      ExecutionStatus::Completed(outputs) => println!("Result: {:?}\n", outputs),
      other => println!("Result: {:?}\n", other),
    }

    println!("=== Demo complete ===");
}
