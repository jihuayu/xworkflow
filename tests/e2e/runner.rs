use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use xworkflow::dsl::{parse_dsl, DslFormat};
use xworkflow::{
    EngineConfig,
    ExecutionStatus,
    FakeIdGenerator,
    FakeTimeProvider,
    RuntimeContext,
    WorkflowRunner,
};

#[derive(Debug, Deserialize, Default)]
struct StateFile {
    #[serde(default)]
    config: Option<EngineConfig>,
    #[serde(default)]
    system_variables: HashMap<String, Value>,
    #[serde(default)]
    environment_variables: HashMap<String, Value>,
    #[serde(default)]
    conversation_variables: HashMap<String, Value>,
    #[serde(default)]
    fake_time: Option<FakeTimeConfig>,
    #[serde(default)]
    fake_id: Option<FakeIdConfig>,
}

#[derive(Debug, Deserialize)]
struct FakeTimeConfig {
    fixed_timestamp: i64,
}

#[derive(Debug, Deserialize)]
struct FakeIdConfig {
    prefix: String,
}

#[derive(Debug, Deserialize)]
struct ExpectedOutput {
    status: String,
    #[serde(default)]
    outputs: HashMap<String, Value>,
    #[serde(default)]
    partial_match: bool,
    #[serde(default)]
    error_contains: Option<String>,
}

pub async fn run_case(case_dir: &Path) {
    let workflow_json = read_to_string(case_dir.join("workflow.json"));
    let inputs: HashMap<String, Value> = read_json(case_dir.join("in.json"));
    let state: StateFile = read_json(case_dir.join("state.json"));
    let expected: ExpectedOutput = read_json(case_dir.join("out.json"));

    let schema = parse_dsl(&workflow_json, DslFormat::Json)
        .unwrap_or_else(|e| panic!("Failed to parse workflow.json: {}", e));

    let mut context = RuntimeContext::default();
    if let Some(fake_time) = state.fake_time {
        context.time_provider = Arc::new(FakeTimeProvider::new(fake_time.fixed_timestamp));
    }
    if let Some(fake_id) = state.fake_id {
        context.id_generator = Arc::new(FakeIdGenerator::new(fake_id.prefix));
    }

    let config = state.config.unwrap_or_default();

    let handle = WorkflowRunner::builder(schema)
        .user_inputs(inputs)
        .system_vars(state.system_variables)
        .environment_vars(state.environment_variables)
        .conversation_vars(state.conversation_variables)
        .config(config)
        .context(context)
        .run()
        .await
        .unwrap_or_else(|e| panic!("Workflow failed to start: {}", e));

    let status = handle.wait().await;
    match expected.status.as_str() {
        "completed" => match status {
            ExecutionStatus::Completed(actual_outputs) => {
                if expected.partial_match {
                    for (k, v) in expected.outputs {
                        assert_eq!(
                            actual_outputs.get(&k),
                            Some(&v),
                            "Output mismatch for key '{}'",
                            k
                        );
                    }
                } else {
                    assert_eq!(
                        actual_outputs, expected.outputs,
                        "Outputs mismatch for case: {}",
                        case_dir.display()
                    );
                }
            }
            other => panic!(
                "Expected completed but got {:?} for case: {}",
                other,
                case_dir.display()
            ),
        },
        "failed" => match status {
            ExecutionStatus::Failed(err) => {
                if let Some(substr) = expected.error_contains {
                    assert!(
                        err.contains(&substr),
                        "Error did not contain '{}': {}",
                        substr,
                        err
                    );
                }
            }
            other => panic!(
                "Expected failed but got {:?} for case: {}",
                other,
                case_dir.display()
            ),
        },
        other => panic!("Unknown expected status: {}", other),
    }
}

fn read_to_string(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path.as_ref()).unwrap_or_else(|e| {
        panic!("Failed to read {}: {}", path.as_ref().display(), e)
    })
}

fn read_json<T: DeserializeOwned>(path: impl AsRef<Path>) -> T {
    let content = read_to_string(path.as_ref());
    serde_json::from_str(&content).unwrap_or_else(|e| {
        panic!("Failed to parse {}: {}", path.as_ref().display(), e)
    })
}
