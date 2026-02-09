use serde_json::json;
use std::sync::Arc;
use tracing_subscriber;

use xworkflow::dsl::DslFormat;
use xworkflow::WorkflowScheduler;

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::fmt().with_env_filter("info").init();

    println!("🚀 XWorkflow Scheduler starting...");

    // Create global scheduler
    let scheduler = Arc::new(WorkflowScheduler::new());
    let _service_handle = scheduler.clone().start();

    // Example workflow DSL
    let workflow_dsl = r#"
nodes:
  - id: start_1
    type: start
    data: {}
    title: Start
  - id: template_1
    type: template
    data:
      template: "Task {{#input.task_id#}}: Process {{#input.data#}}"
    title: Process Template
  - id: end_1
    type: end
    data:
      outputs:
        - name: result
          variable_selector: "template_1.output"
    title: End
edges:
  - source: start_1
    target: template_1
  - source: template_1
    target: end_1
"#;

    println!("📋 Submitting 5 concurrent workflow tasks...");

    // Submit multiple workflows concurrently
    let mut handles = Vec::new();
    for i in 1..=5 {
        let inputs = json!({
            "task_id": i,
            "data": format!("Dataset_{}", i)
        });

        let handle = scheduler
            .submit(workflow_dsl, DslFormat::Yaml, inputs, "demo_user")
            .await
            .expect("Failed to submit workflow");

        println!(
            "✅ Task {} submitted (execution_id: {})",
            i, handle.execution_id
        );
        handles.push((i, handle));
    }

    println!("\n⏳ Waiting for all tasks to complete...\n");

    // Wait for all tasks to complete and print results
    for (task_id, handle) in handles {
        match handle.wait().await {
            Ok(output) => {
                println!(
                    "✅ Task {} completed: {}",
                    task_id,
                    output.get("result").unwrap_or(&json!("N/A"))
                );
            }
            Err(e) => {
                eprintln!("❌ Task {} failed: {}", task_id, e);
            }
        }
    }

    println!("\n🎉 All tasks completed!");

    // Branching workflow demo
    println!("\n{}", "=".repeat(60));
    println!("📋 Submitting branching workflow tasks...\n");

    let branch_workflow = r#"
nodes:
  - id: start_1
    type: start
    data: {}
  - id: ifelse_1
    type: if-else
    data:
      conditions:
        - variable_selector: "input.score"
          comparison_operator: greater_than_or_equal
          value: 60
      logical_operator: and
  - id: pass_template
    type: template
    data:
      template: "{{#input.name#}} passed the exam! Score: {{#input.score#}}"
  - id: fail_template
    type: template
    data:
      template: "{{#input.name#}} did not pass the exam, Score: {{#input.score#}}"
  - id: end_1
    type: end
    data:
      outputs:
        - name: result
          variable_selector: "pass_template.output"
        - name: result2
          variable_selector: "fail_template.output"
edges:
  - source: start_1
    target: ifelse_1
  - source: ifelse_1
    target: pass_template
    source_handle: "true"
  - source: ifelse_1
    target: fail_template
    source_handle: "false"
  - source: pass_template
    target: end_1
  - source: fail_template
    target: end_1
"#;

    let test_cases = vec![
        ("Zhang San", 85),
        ("Li Si", 45),
        ("Wang Wu", 72),
        ("Zhao Liu", 58),
    ];

    for (name, score) in test_cases {
        let handle = scheduler
            .submit(
                branch_workflow,
                DslFormat::Yaml,
                json!({"name": name, "score": score}),
                "demo_user",
            )
            .await
            .expect("Failed to submit workflow");

        let output = handle.wait().await.expect("Workflow failed");
        let default = json!("N/A");
        let result = output
            .get("result")
            .or_else(|| output.get("result2"))
            .unwrap_or(&default);
        println!("📊 {}", result);
    }

    println!("\n✨ Scheduler demo completed!");
}
