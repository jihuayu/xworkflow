use serde_json::json;
use tracing_subscriber;

use xworkflow::dsl::DslFormat;

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    // 示例工作流 DSL
    let dsl = r#"
nodes:
  - id: start_1
    type: start
    data: {}
    title: Start
  - id: template_1
    type: template
    data:
      template: "Hello {{#input.name#}}! Welcome to XWorkflow."
    title: Greeting
  - id: end_1
    type: end
    data:
      outputs:
        - name: greeting
          variable_selector: "template_1.output"
    title: End

edges:
  - source: start_1
    target: template_1
  - source: template_1
    target: end_1
"#;

    let inputs = json!({"name": "World"});

    println!("🚀 XWorkflow Engine Starting...");
    println!("📄 Parsing workflow DSL...");

    match xworkflow::execute_workflow_sync(dsl, DslFormat::Yaml, inputs, "demo_user").await {
        Ok(outputs) => {
            println!("✅ Workflow completed successfully!");
            println!("📦 Outputs: {}", serde_json::to_string_pretty(&outputs).unwrap());
        }
        Err(e) => {
            eprintln!("❌ Workflow failed: {}", e);
            std::process::exit(1);
        }
    }
}
