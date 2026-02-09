use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use xworkflow::core::debug::{DebugConfig, DebugEvent, PauseLocation, PauseReason};
use xworkflow::dsl::{parse_dsl, DslFormat};
use xworkflow::{
    plugin::{AllowedCapabilities, PluginManager, PluginManagerConfig},
    EngineConfig,
    ExecutionStatus,
    FakeIdGenerator,
    FakeTimeProvider,
    RuntimeContext,
    WorkflowRunner,
};
use xworkflow::llm::{LlmProviderRegistry, OpenAiConfig, OpenAiProvider};

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
    #[serde(default)]
    plugin_dir: Option<String>,
    #[serde(default)]
    llm_providers: Option<LlmProvidersConfig>,
    #[serde(default)]
    mock_server: Option<Vec<MockEndpoint>>,
}

#[derive(Debug, Deserialize, Default)]
struct LlmProvidersConfig {
    #[serde(default)]
    openai: Option<OpenAiProviderConfig>,
}

#[derive(Debug, Deserialize)]
struct OpenAiProviderConfig {
    api_key: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    org_id: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FakeTimeConfig {
    fixed_timestamp: i64,
}

#[derive(Debug, Deserialize)]
struct FakeIdConfig {
    prefix: String,
}

/// Defines a mock HTTP endpoint for testing HTTP request nodes.
#[derive(Debug, Deserialize)]
struct MockEndpoint {
    /// HTTP method: GET, POST, PUT, DELETE, PATCH, HEAD
    method: String,
    /// URL path, e.g. "/api/data"
    path: String,
    /// Response HTTP status code
    #[serde(default = "default_status")]
    response_status: usize,
    /// Response headers
    #[serde(default)]
    response_headers: HashMap<String, String>,
    /// Response body string
    #[serde(default)]
    response_body: String,
    /// Expected request headers (optional, for matching)
    #[serde(default)]
    match_headers: HashMap<String, String>,
    /// Expected request body substring (optional, for matching)
    #[serde(default)]
    match_body: Option<String>,
}

fn default_status() -> usize {
    200
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
    let mut state: StateFile = read_json(case_dir.join("state.json"));
    let expected: ExpectedOutput = read_json(case_dir.join("out.json"));

    // Set up mock HTTP server if configured
    let _mock_server_guard = setup_mock_server(&mut state).await;

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

    let sys_base_url = state
        .system_variables
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut builder = WorkflowRunner::builder(schema)
        .user_inputs(inputs)
        .system_vars(state.system_variables)
        .environment_vars(state.environment_variables)
        .conversation_vars(state.conversation_variables)
        .config(config)
        .context(context);

    if let Some(llm_config) = state.llm_providers {
        let mut registry = LlmProviderRegistry::new();
        if let Some(openai) = llm_config.openai {
            let base_url = if let Some(url) = openai.base_url.clone() {
                url
            } else if let Some(base_url) = &sys_base_url {
                format!("{}/v1", base_url.trim_end_matches('/'))
            } else {
                "https://api.openai.com/v1".to_string()
            };
            registry.register(Arc::new(OpenAiProvider::new(OpenAiConfig {
                api_key: openai.api_key,
                base_url,
                org_id: openai.org_id,
                default_model: openai.default_model.unwrap_or_else(|| "gpt-4o".into()),
            })));
        }
        builder = builder.llm_providers(Arc::new(registry));
    }

    if let Some(plugin_dir) = state.plugin_dir {
        let plugin_path = if Path::new(&plugin_dir).is_absolute() {
            PathBuf::from(plugin_dir)
        } else {
            case_dir.join(plugin_dir)
        };
        let manager = PluginManager::new(PluginManagerConfig {
            plugin_dir: plugin_path,
            auto_discover: true,
            default_max_memory_pages: 64,
            default_max_fuel: 100_000,
            allowed_capabilities: AllowedCapabilities {
                read_variables: true,
                write_variables: true,
                emit_events: true,
                http_access: false,
                fs_access: false,
            },
        })
        .unwrap();
        builder = builder.plugin_manager(Arc::new(manager));
    }

    let handle = builder.run().await;
    if handle.is_err() {
        let err = handle.err().unwrap();
        if expected.status == "failed" {
            if let Some(substr) = expected.error_contains {
                assert!(
                    err.to_string().contains(&substr),
                    "Error did not contain '{}': {}",
                    substr,
                    err
                );
            }
            return;
        }
        panic!("Workflow failed to start: {}", err);
    }

    let status = handle.unwrap().wait().await;
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
        "failed_with_recovery" => match status {
            ExecutionStatus::FailedWithRecovery { original_error, recovered_outputs } => {
                if let Some(substr) = expected.error_contains {
                    assert!(
                        original_error.contains(&substr),
                        "Error did not contain '{}': {}",
                        substr,
                        original_error
                    );
                }
                if expected.partial_match {
                    for (k, v) in expected.outputs {
                        assert_eq!(
                            recovered_outputs.get(&k),
                            Some(&v),
                            "Output mismatch for key '{}'",
                            k
                        );
                    }
                } else {
                    assert_eq!(
                        recovered_outputs, expected.outputs,
                        "Outputs mismatch for case: {}",
                        case_dir.display()
                    );
                }
            }
            other => panic!(
                "Expected failed_with_recovery but got {:?} for case: {}",
                other,
                case_dir.display()
            ),
        },
        other => panic!("Unknown expected status: {}", other),
    }
}

/// A guard that keeps the mock server and its mocks alive for the test duration.
struct MockServerGuard {
    _server: mockito::ServerGuard,
    _mocks: Vec<mockito::Mock>,
}

// =====================================================================
// Debug mode e2e test support
// =====================================================================

/// Debug test configuration loaded from `debug.json`
#[derive(Debug, Deserialize)]
struct DebugTestFile {
    /// Debug configuration: breakpoints, break_on_start, auto_snapshot
    #[serde(default)]
    config: DebugConfigFile,
    /// Ordered list of debug steps to execute at each pause
    steps: Vec<DebugStep>,
}

#[derive(Debug, Deserialize, Default)]
struct DebugConfigFile {
    #[serde(default)]
    breakpoints: Vec<String>,
    #[serde(default)]
    break_on_start: bool,
    #[serde(default)]
    auto_snapshot: bool,
}

/// A single debug interaction step
#[derive(Debug, Deserialize)]
struct DebugStep {
    /// Expected node_id at pause (optional — skip check if absent)
    #[serde(default)]
    expect_node: Option<String>,
    /// Expected pause reason: "breakpoint", "step", "initial" (optional)
    #[serde(default)]
    expect_reason: Option<String>,
    /// Expected pause phase: "before" or "after" (optional)
    #[serde(default)]
    expect_phase: Option<String>,
    /// Action to take: "step", "continue", "abort", "inspect", "update_variables"
    action: String,
    /// For "abort" action: optional reason
    #[serde(default)]
    abort_reason: Option<String>,
    /// For "update_variables" action: variables to update (key format "node_id.var_name")
    #[serde(default)]
    variables: Option<HashMap<String, Value>>,
    /// For "add_breakpoint" action: node_id to add breakpoint on
    #[serde(default)]
    add_breakpoint: Option<String>,
    /// For "remove_breakpoint" action: node_id to remove breakpoint from
    #[serde(default)]
    remove_breakpoint: Option<String>,
    /// Whether to verify that a variable snapshot event is received after this step
    #[serde(default)]
    expect_snapshot: bool,
    /// Optional: expected variables in the snapshot (subset match)
    #[serde(default)]
    expect_snapshot_contains: Option<HashMap<String, Value>>,
}

pub async fn run_debug_case(case_dir: &Path) {
    let workflow_json = read_to_string(case_dir.join("workflow.json"));
    let inputs: HashMap<String, Value> = read_json(case_dir.join("in.json"));
    let mut state: StateFile = read_json(case_dir.join("state.json"));
    let expected: ExpectedOutput = read_json(case_dir.join("out.json"));
    let debug_test: DebugTestFile = read_json(case_dir.join("debug.json"));

    // Set up mock HTTP server if configured
    let _mock_server_guard = setup_mock_server(&mut state).await;

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

    // Build DebugConfig from debug.json
    let mut dbg_config = DebugConfig::default();
    dbg_config.break_on_start = debug_test.config.break_on_start;
    dbg_config.auto_snapshot = debug_test.config.auto_snapshot;
    for bp in &debug_test.config.breakpoints {
        dbg_config.breakpoints.insert(bp.clone());
    }

    let sys_base_url = state
        .system_variables
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut builder = WorkflowRunner::builder(schema)
        .user_inputs(inputs)
        .system_vars(state.system_variables)
        .environment_vars(state.environment_variables)
        .conversation_vars(state.conversation_variables)
        .config(config)
        .context(context)
        .debug(dbg_config);

    if let Some(llm_config) = state.llm_providers {
        let mut registry = LlmProviderRegistry::new();
        if let Some(openai) = llm_config.openai {
            let base_url = if let Some(url) = openai.base_url.clone() {
                url
            } else if let Some(base_url) = &sys_base_url {
                format!("{}/v1", base_url.trim_end_matches('/'))
            } else {
                "https://api.openai.com/v1".to_string()
            };
            registry.register(Arc::new(OpenAiProvider::new(OpenAiConfig {
                api_key: openai.api_key,
                base_url,
                org_id: openai.org_id,
                default_model: openai.default_model.unwrap_or_else(|| "gpt-4o".into()),
            })));
        }
        builder = builder.llm_providers(Arc::new(registry));
    }

    if let Some(plugin_dir) = state.plugin_dir {
        let plugin_path = if Path::new(&plugin_dir).is_absolute() {
            PathBuf::from(plugin_dir)
        } else {
            case_dir.join(plugin_dir)
        };
        let manager = PluginManager::new(PluginManagerConfig {
            plugin_dir: plugin_path,
            auto_discover: true,
            default_max_memory_pages: 64,
            default_max_fuel: 100_000,
            allowed_capabilities: AllowedCapabilities {
                read_variables: true,
                write_variables: true,
                emit_events: true,
                http_access: false,
                fs_access: false,
            },
        })
        .unwrap();
        builder = builder.plugin_manager(Arc::new(manager));
    }

    let (handle, debug) = builder
        .run_debug()
        .await
        .unwrap_or_else(|e| panic!("run_debug failed: {}", e));

    // Drive debug steps
    for (i, step) in debug_test.steps.iter().enumerate() {
        // For actions that require waiting for a pause first
        let needs_pause = matches!(
            step.action.as_str(),
            "step" | "continue" | "abort" | "inspect" | "update_variables"
                | "add_breakpoint" | "remove_breakpoint"
        );

        if needs_pause {
            let pause_event = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                debug.wait_for_pause(),
            )
            .await
            .unwrap_or_else(|_| panic!("Step {}: Timed out waiting for pause", i))
            .unwrap_or_else(|e| panic!("Step {}: wait_for_pause error: {:?}", i, e));

            // Verify pause expectations
            if let DebugEvent::Paused { reason, location } = &pause_event {
                if let Some(expected_node) = &step.expect_node {
                    let actual_node = match location {
                        PauseLocation::BeforeNode { node_id, .. } => node_id,
                        PauseLocation::AfterNode { node_id, .. } => node_id,
                    };
                    assert_eq!(
                        actual_node, expected_node,
                        "Step {}: expected pause at node '{}', got '{}'",
                        i, expected_node, actual_node
                    );
                }

                if let Some(expected_reason) = &step.expect_reason {
                    let actual_reason = match reason {
                        PauseReason::Breakpoint => "breakpoint",
                        PauseReason::Step => "step",
                        PauseReason::Initial => "initial",
                        PauseReason::UserRequested => "user_requested",
                    };
                    assert_eq!(
                        actual_reason, expected_reason.as_str(),
                        "Step {}: expected reason '{}', got '{}'",
                        i, expected_reason, actual_reason
                    );
                }

                if let Some(expected_phase) = &step.expect_phase {
                    let actual_phase = match location {
                        PauseLocation::BeforeNode { .. } => "before",
                        PauseLocation::AfterNode { .. } => "after",
                    };
                    assert_eq!(
                        actual_phase, expected_phase.as_str(),
                        "Step {}: expected phase '{}', got '{}'",
                        i, expected_phase, actual_phase
                    );
                }
            }
        }

        // Execute the action
        match step.action.as_str() {
            "step" => {
                // Optionally add/remove breakpoints before stepping
                if let Some(bp) = &step.add_breakpoint {
                    debug.add_breakpoint(bp).await.unwrap();
                }
                if let Some(bp) = &step.remove_breakpoint {
                    debug.remove_breakpoint(bp).await.unwrap();
                }
                debug.step().await.unwrap_or_else(|e| {
                    panic!("Step {}: step command failed: {:?}", i, e)
                });
            }
            "continue" => {
                if let Some(bp) = &step.add_breakpoint {
                    debug.add_breakpoint(bp).await.unwrap();
                }
                if let Some(bp) = &step.remove_breakpoint {
                    debug.remove_breakpoint(bp).await.unwrap();
                }
                debug.continue_run().await.unwrap_or_else(|e| {
                    panic!("Step {}: continue command failed: {:?}", i, e)
                });
            }
            "abort" => {
                debug
                    .abort(step.abort_reason.clone())
                    .await
                    .unwrap_or_else(|e| {
                        panic!("Step {}: abort command failed: {:?}", i, e)
                    });
            }
            "inspect" => {
                debug.inspect_variables().await.unwrap_or_else(|e| {
                    panic!("Step {}: inspect command failed: {:?}", i, e)
                });

                if step.expect_snapshot {
                    let snapshot_event = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        async {
                            loop {
                                match debug.next_event().await {
                                    Some(DebugEvent::VariableSnapshot { variables }) => {
                                        return variables
                                    }
                                    Some(_) => continue,
                                    None => panic!("Step {}: channel closed waiting for snapshot", i),
                                }
                            }
                        },
                    )
                    .await
                    .unwrap_or_else(|_| panic!("Step {}: Timed out waiting for snapshot", i));

                    if let Some(expected_vars) = &step.expect_snapshot_contains {
                        for (key, expected_val) in expected_vars {
                            let parts: Vec<&str> = key.splitn(2, '.').collect();
                            if parts.len() == 2 {
                                let pool_key = (parts[0].to_string(), parts[1].to_string());
                                let actual = snapshot_event.get(&pool_key);
                                assert!(
                                    actual.is_some(),
                                    "Step {}: snapshot missing key '{}'",
                                    i, key
                                );
                                let actual_val = actual.unwrap().to_value();
                                assert_eq!(
                                    &actual_val, expected_val,
                                    "Step {}: snapshot value mismatch for key '{}'",
                                    i, key
                                );
                            }
                        }
                    }
                }

                // After inspect, we need to send an action to resume — use step or continue
                // The inspect action alone doesn't resume execution, the next step will handle that
                continue;
            }
            "update_variables" => {
                let vars = step.variables.clone().unwrap_or_default();
                debug
                    .update_variables(vars)
                    .await
                    .unwrap_or_else(|e| {
                        panic!("Step {}: update_variables command failed: {:?}", i, e)
                    });
                // UpdateVariables returns an action that continues execution
            }
            other => panic!("Step {}: unknown action '{}'", i, other),
        }
    }

    // Wait for workflow completion
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        handle.wait(),
    )
    .await
    .unwrap_or_else(|_| panic!("Timed out waiting for workflow completion"));

    // Verify final outcome
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
                        "Outputs mismatch for debug case: {}",
                        case_dir.display()
                    );
                }
            }
            other => panic!(
                "Expected completed but got {:?} for debug case: {}",
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
                "Expected failed but got {:?} for debug case: {}",
                other,
                case_dir.display()
            ),
        },
        other => panic!("Unknown expected status: {}", other),
    }
}

/// Set up a mock HTTP server based on the state file configuration.
/// Injects `sys.base_url` into system_variables so workflows can reference it.
async fn setup_mock_server(state: &mut StateFile) -> Option<MockServerGuard> {
    let endpoints = state.mock_server.take()?;
    if endpoints.is_empty() {
        return None;
    }

    let mut server = mockito::Server::new_async().await;
    let base_url = server.url();

    // Inject base_url into system variables
    state
        .system_variables
        .insert("base_url".to_string(), Value::String(base_url));

    let mut mocks = Vec::new();
    for ep in endpoints {
        let mut mock = server.mock(ep.method.as_str(), ep.path.as_str());

        // Set up request matchers
        for (key, value) in &ep.match_headers {
            mock = mock.match_header(key.as_str(), value.as_str());
        }
        if let Some(body_pattern) = &ep.match_body {
            mock = mock.match_body(mockito::Matcher::Regex(body_pattern.clone()));
        }

        // Set up response
        mock = mock.with_status(ep.response_status);
        for (key, value) in &ep.response_headers {
            mock = mock.with_header(key.as_str(), value.as_str());
        }
        mock = mock.with_body(&ep.response_body);

        let created_mock = mock.create_async().await;
        mocks.push(created_mock);
    }

    Some(MockServerGuard {
        _server: server,
        _mocks: mocks,
    })
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
