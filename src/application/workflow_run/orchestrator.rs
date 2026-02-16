use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex, RwLock};

use crate::application::bootstrap::{SchedulerPluginGate, SchedulerSecurityGate};
use crate::compiler::CompiledNodeConfigMap;
use crate::core::debug::{
    DebugConfig, DebugHandle, InteractiveDebugGate, InteractiveDebugHook, StepMode,
};
use crate::core::dispatcher::{EngineConfig, EventEmitter, WorkflowDispatcher};
use crate::core::event_bus::GraphEngineEvent;
use crate::core::sub_graph_runner::DefaultSubGraphRunner;
use crate::core::variable_pool::{Segment, SegmentType, VariablePool};
use crate::core::workflow_context::WorkflowContext;
use crate::core::SafeStopSignal;
use crate::domain::execution::ExecutionStatus;
use crate::dsl::schema::{ErrorHandlingMode, WorkflowSchema};
use crate::error::WorkflowError;
use crate::graph::{build_graph, Graph, GraphTopology};
use crate::llm::LlmProviderRegistry;
#[cfg(feature = "builtin-agent-node")]
use crate::mcp::pool::McpConnectionPool;
use crate::nodes::executor::NodeExecutorRegistry;

#[cfg(feature = "checkpoint")]
use crate::core::checkpoint::{CheckpointStore, ResumePolicy};

use super::{segment_from_type, WorkflowHandle};

pub(crate) enum WorkflowGraphSpec {
    FromSchema,
    FromTopology(Arc<GraphTopology>),
}

pub(crate) struct WorkflowRunSpec {
    pub(crate) schema: Arc<WorkflowSchema>,
    pub(crate) graph_spec: WorkflowGraphSpec,
    pub(crate) start_var_types: Arc<HashMap<String, SegmentType>>,
    pub(crate) conversation_var_types: Arc<HashMap<String, SegmentType>>,
    pub(crate) compiled_node_configs: Option<Arc<CompiledNodeConfigMap>>,
}

pub(crate) struct WorkflowRunOptions {
    pub(crate) user_inputs: HashMap<String, Value>,
    pub(crate) system_vars: HashMap<String, Value>,
    pub(crate) environment_vars: HashMap<String, Value>,
    pub(crate) conversation_vars: HashMap<String, Value>,
    pub(crate) config: EngineConfig,
    pub(crate) context: WorkflowContext,
    pub(crate) plugin_gate: Box<dyn SchedulerPluginGate>,
    pub(crate) security_gate: Arc<dyn SchedulerSecurityGate>,
    pub(crate) llm_provider_registry: Option<Arc<LlmProviderRegistry>>,
    pub(crate) collect_events: bool,
    #[cfg(feature = "checkpoint")]
    pub(crate) checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    #[cfg(feature = "checkpoint")]
    pub(crate) workflow_id: Option<String>,
    pub(crate) safe_stop_signal: Option<SafeStopSignal>,
    #[cfg(feature = "checkpoint")]
    pub(crate) resume_policy: ResumePolicy,
}

struct PreparedExecution {
    schema: Arc<WorkflowSchema>,
    graph: Graph,
    pool: VariablePool,
    registry: Arc<NodeExecutorRegistry>,
    context: WorkflowContext,
    config: EngineConfig,
    workflow_id: Option<String>,
    compiled_node_configs: Option<Arc<CompiledNodeConfigMap>>,
    plugin_registry: PluginRegistryForRun,
    security_gate: Arc<dyn SchedulerSecurityGate>,
    collect_events: bool,
    safe_stop_signal: Option<SafeStopSignal>,
    #[cfg(feature = "checkpoint")]
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    #[cfg(feature = "checkpoint")]
    workflow_id_override: Option<String>,
    #[cfg(feature = "checkpoint")]
    resume_policy: ResumePolicy,
}

#[cfg(feature = "plugin-system")]
type PluginRegistryForRun = Option<Arc<crate::plugin_system::PluginRegistry>>;
#[cfg(not(feature = "plugin-system"))]
type PluginRegistryForRun = ();

pub(crate) async fn run_workflow(
    spec: WorkflowRunSpec,
    options: WorkflowRunOptions,
) -> Result<WorkflowHandle, WorkflowError> {
    let prepared = prepare_execution(spec, options).await?;
    run_prepared(prepared).await
}

pub(crate) async fn run_workflow_debug(
    spec: WorkflowRunSpec,
    options: WorkflowRunOptions,
    debug_config: DebugConfig,
) -> Result<(WorkflowHandle, DebugHandle), WorkflowError> {
    let prepared = prepare_execution(spec, options).await?;
    run_prepared_debug(prepared, debug_config).await
}

async fn prepare_execution(
    spec: WorkflowRunSpec,
    options: WorkflowRunOptions,
) -> Result<PreparedExecution, WorkflowError> {
    let WorkflowRunSpec {
        schema,
        graph_spec,
        start_var_types,
        conversation_var_types,
        compiled_node_configs,
    } = spec;
    let WorkflowRunOptions {
        user_inputs,
        mut system_vars,
        environment_vars,
        conversation_vars,
        config,
        mut context,
        mut plugin_gate,
        security_gate,
        llm_provider_registry,
        collect_events,
        #[cfg(feature = "checkpoint")]
        checkpoint_store,
        #[cfg(feature = "checkpoint")]
            workflow_id: workflow_id_override,
        safe_stop_signal,
        #[cfg(feature = "checkpoint")]
        resume_policy,
    } = options;

    let mut report = security_gate.validate_schema(schema.as_ref(), &context);
    if !report.is_valid {
        security_gate
            .audit_validation_failed(&context, &report)
            .await;
        return Err(WorkflowError::ValidationFailed(Box::new(report)));
    }

    plugin_gate
        .init_and_extend_validation(schema.as_ref(), &mut report)
        .await?;
    plugin_gate.after_dsl_validation(&report).await?;

    if !report.is_valid {
        security_gate
            .audit_validation_failed(&context, &report)
            .await;
        return Err(WorkflowError::ValidationFailed(Box::new(report)));
    }

    let graph = match graph_spec {
        WorkflowGraphSpec::FromSchema => build_graph(schema.as_ref())?,
        WorkflowGraphSpec::FromTopology(topology) => Graph::from_topology(topology),
    };

    let mut pool = VariablePool::new();
    security_gate.configure_variable_pool(&context, &mut pool);

    #[cfg(feature = "builtin-agent-node")]
    if !schema.mcp_servers.is_empty() {
        system_vars
            .entry("__mcp_servers".to_string())
            .or_insert_with(|| {
                serde_json::to_value(&schema.mcp_servers)
                    .unwrap_or(Value::Object(serde_json::Map::new()))
            });
    }

    for (k, v) in &system_vars {
        let selector = crate::core::variable_pool::Selector::new("sys", k.clone());
        pool.set(&selector, Segment::from_value(v));
    }

    for (k, v) in &environment_vars {
        let selector = crate::core::variable_pool::Selector::new("env", k.clone());
        pool.set(&selector, Segment::from_value(v));
    }

    for (k, v) in &conversation_vars {
        let selector = crate::core::variable_pool::Selector::new("conversation", k.clone());
        let seg = segment_from_type(v, conversation_var_types.get(k));
        pool.set(&selector, seg);
    }

    let start_node_id = graph.root_node_id().to_string();
    for (k, v) in &user_inputs {
        let selector = crate::core::variable_pool::Selector::new(start_node_id.clone(), k.clone());
        let seg = segment_from_type(v, start_var_types.get(k));
        pool.set(&selector, seg);
    }

    let mut registry = NodeExecutorRegistry::new();
    plugin_gate.apply_node_executors(&mut registry);

    let mut llm_registry = if let Some(llm_reg) = &llm_provider_registry {
        llm_reg.clone_registry()
    } else {
        LlmProviderRegistry::new()
    };
    plugin_gate.apply_llm_providers(&mut llm_registry);

    let llm_registry = Arc::new(llm_registry);
    registry.set_llm_provider_registry(Arc::clone(&llm_registry));
    #[cfg(feature = "builtin-agent-node")]
    {
        let mcp_pool = Arc::new(RwLock::new(McpConnectionPool::new()));
        registry.set_mcp_pool(mcp_pool);
    }

    let registry = Arc::new(registry);
    context = context.with_node_executor_registry(Arc::clone(&registry));
    context = context.with_llm_provider_registry(Arc::clone(&llm_registry));

    let config = security_gate.effective_engine_config(&context, config);
    context.strict_template = config.strict_template;

    let workflow_id = security_gate.on_workflow_start(&context).await?;
    context.workflow_id = workflow_id.clone();
    plugin_gate.customize_context(&mut context);

    #[cfg(feature = "plugin-system")]
    let plugin_registry = plugin_gate.take_plugin_registry_arc();
    #[cfg(not(feature = "plugin-system"))]
    let plugin_registry = ();

    Ok(PreparedExecution {
        schema,
        graph,
        pool,
        registry,
        context,
        config,
        workflow_id,
        compiled_node_configs,
        plugin_registry,
        security_gate,
        collect_events,
        safe_stop_signal,
        #[cfg(feature = "checkpoint")]
        checkpoint_store,
        #[cfg(feature = "checkpoint")]
        workflow_id_override,
        #[cfg(feature = "checkpoint")]
        resume_policy,
    })
}

async fn run_prepared(prepared: PreparedExecution) -> Result<WorkflowHandle, WorkflowError> {
    let PreparedExecution {
        schema,
        graph,
        pool,
        registry,
        context,
        config,
        workflow_id,
        compiled_node_configs,
        plugin_registry,
        security_gate,
        collect_events,
        safe_stop_signal,
        #[cfg(feature = "checkpoint")]
        checkpoint_store,
        #[cfg(feature = "checkpoint")]
        workflow_id_override,
        #[cfg(feature = "checkpoint")]
        resume_policy,
    } = prepared;

    let (tx, mut rx) = mpsc::channel(256);
    let event_active = Arc::new(AtomicBool::new(collect_events));
    let event_emitter = EventEmitter::new(tx.clone(), event_active.clone());
    let error_event_emitter = event_emitter.clone();

    let context = Arc::new(context.with_event_tx(tx.clone()));
    let (command_tx, command_rx) = mpsc::channel(64);

    #[cfg(feature = "checkpoint")]
    let checkpoint_workflow_id = workflow_id_override
        .clone()
        .or_else(|| workflow_id.clone())
        .or_else(|| Some(context.execution_id.clone()));
    #[cfg(feature = "checkpoint")]
    let checkpoint_execution_id = Some(context.execution_id.clone());
    #[cfg(feature = "checkpoint")]
    let checkpoint_resume_policy = resume_policy;

    #[cfg(feature = "checkpoint")]
    let checkpoint_schema_hash = crate::core::checkpoint::hash_json(schema.as_ref());
    #[cfg(feature = "checkpoint")]
    let checkpoint_engine_config_hash = crate::core::checkpoint::hash_json(&config);

    let (status_tx, status_rx) = watch::channel(ExecutionStatus::Running);
    let events = if collect_events {
        Some(Arc::new(Mutex::new(Vec::new())))
    } else {
        None
    };

    if let Some(events_clone) = events.clone() {
        let active_flag = event_active.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                events_clone.lock().await.push(event);
            }
            active_flag.store(false, Ordering::Relaxed);
        });
    } else {
        event_active.store(false, Ordering::Relaxed);
        drop(rx);
    }

    let status_exec = status_tx.clone();
    let workflow_id_for_end = workflow_id.clone();
    #[cfg(feature = "plugin-system")]
    let plugin_registry_for_shutdown = plugin_registry.clone();

    tokio::spawn(async move {
        let mut dispatcher = if let Some(compiled) = compiled_node_configs {
            WorkflowDispatcher::new_with_registry_and_compiled(
                graph,
                pool,
                registry,
                event_emitter,
                config,
                context.clone(),
                compiled,
                #[cfg(feature = "plugin-system")]
                plugin_registry,
            )
        } else {
            WorkflowDispatcher::new_with_registry(
                graph,
                pool,
                registry,
                event_emitter,
                config,
                context.clone(),
                #[cfg(feature = "plugin-system")]
                plugin_registry,
            )
        };
        dispatcher.set_control_channels(status_exec.clone(), command_rx);
        dispatcher.set_safe_stop_signal(safe_stop_signal);
        #[cfg(feature = "checkpoint")]
        dispatcher.set_checkpoint_options(
            checkpoint_store,
            checkpoint_workflow_id,
            checkpoint_execution_id,
            checkpoint_resume_policy,
            checkpoint_schema_hash,
            checkpoint_engine_config_hash,
        );
        match dispatcher.run().await {
            Ok(outputs) => {
                let _ = status_exec.send(ExecutionStatus::Completed(outputs));
            }
            Err(e) => {
                if let WorkflowError::SafeStopped {
                    last_completed_node,
                    interrupted_nodes,
                    checkpoint_saved,
                } = &e
                {
                    let _ = status_exec.send(ExecutionStatus::SafeStopped {
                        last_completed_node: last_completed_node.clone(),
                        interrupted_nodes: interrupted_nodes.clone(),
                        checkpoint_saved: *checkpoint_saved,
                    });
                    return;
                }

                handle_error_with_optional_handler(
                    schema.as_ref(),
                    &dispatcher,
                    &context,
                    &status_exec,
                    &error_event_emitter,
                    e,
                )
                .await;
            }
        }

        security_gate
            .record_workflow_end(context.as_ref(), workflow_id_for_end.as_deref())
            .await;

        #[cfg(feature = "plugin-system")]
        if let Some(registry) = plugin_registry_for_shutdown {
            let _ = registry.shutdown_all().await;
        }
    });

    Ok(WorkflowHandle::new(
        status_rx,
        events,
        event_active,
        command_tx,
    ))
}

async fn run_prepared_debug(
    prepared: PreparedExecution,
    debug_config: DebugConfig,
) -> Result<(WorkflowHandle, DebugHandle), WorkflowError> {
    let PreparedExecution {
        schema,
        graph,
        pool,
        registry,
        context,
        config,
        workflow_id,
        compiled_node_configs,
        plugin_registry,
        security_gate,
        collect_events,
        safe_stop_signal,
        #[cfg(feature = "checkpoint")]
        checkpoint_store,
        #[cfg(feature = "checkpoint")]
        workflow_id_override,
        #[cfg(feature = "checkpoint")]
        resume_policy,
    } = prepared;

    let (tx, mut rx) = mpsc::channel(256);
    let event_active = Arc::new(AtomicBool::new(collect_events));
    let event_emitter = EventEmitter::new(tx.clone(), event_active.clone());
    let error_event_emitter = event_emitter.clone();

    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (debug_evt_tx, debug_evt_rx) = mpsc::channel(256);

    let config_arc = Arc::new(RwLock::new(debug_config.clone()));
    let mode_arc = Arc::new(RwLock::new(if debug_config.break_on_start {
        StepMode::Initial
    } else {
        StepMode::Run
    }));

    let gate = InteractiveDebugGate {
        config: config_arc.clone(),
        mode: mode_arc.clone(),
    };
    let hook = InteractiveDebugHook {
        cmd_rx: Mutex::new(cmd_rx),
        event_tx: debug_evt_tx,
        graph_event_tx: Some(tx.clone()),
        config: config_arc,
        mode: mode_arc,
        last_pause: Arc::new(RwLock::new(None)),
        step_count: Arc::new(RwLock::new(0)),
    };

    let context = Arc::new(context.with_event_tx(tx.clone()));
    let (command_tx, command_rx) = mpsc::channel(64);

    #[cfg(feature = "checkpoint")]
    let checkpoint_workflow_id = workflow_id_override
        .clone()
        .or_else(|| workflow_id.clone())
        .or_else(|| Some(context.execution_id.clone()));
    #[cfg(feature = "checkpoint")]
    let checkpoint_execution_id = Some(context.execution_id.clone());
    #[cfg(feature = "checkpoint")]
    let checkpoint_resume_policy = resume_policy;

    #[cfg(feature = "checkpoint")]
    let checkpoint_schema_hash = crate::core::checkpoint::hash_json(schema.as_ref());
    #[cfg(feature = "checkpoint")]
    let checkpoint_engine_config_hash = crate::core::checkpoint::hash_json(&config);

    let (status_tx, status_rx) = watch::channel(ExecutionStatus::Running);
    let events = if collect_events {
        Some(Arc::new(Mutex::new(Vec::new())))
    } else {
        None
    };

    if let Some(events_clone) = events.clone() {
        let active_flag = event_active.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                events_clone.lock().await.push(event);
            }
            active_flag.store(false, Ordering::Relaxed);
        });
    } else {
        event_active.store(false, Ordering::Relaxed);
        drop(rx);
    }

    let status_exec = status_tx.clone();
    let workflow_id_for_end = workflow_id.clone();
    #[cfg(feature = "plugin-system")]
    let plugin_registry_for_shutdown = plugin_registry.clone();

    tokio::spawn(async move {
        let mut dispatcher = if let Some(compiled) = compiled_node_configs {
            WorkflowDispatcher::new_with_debug_and_compiled(
                graph,
                pool,
                registry,
                event_emitter,
                config,
                context.clone(),
                compiled,
                #[cfg(feature = "plugin-system")]
                plugin_registry,
                gate,
                hook,
            )
        } else {
            WorkflowDispatcher::new_with_debug(
                graph,
                pool,
                registry,
                event_emitter,
                config,
                context.clone(),
                #[cfg(feature = "plugin-system")]
                plugin_registry,
                gate,
                hook,
            )
        };
        dispatcher.set_control_channels(status_exec.clone(), command_rx);
        dispatcher.set_safe_stop_signal(safe_stop_signal);
        #[cfg(feature = "checkpoint")]
        dispatcher.set_checkpoint_options(
            checkpoint_store,
            checkpoint_workflow_id,
            checkpoint_execution_id,
            checkpoint_resume_policy,
            checkpoint_schema_hash,
            checkpoint_engine_config_hash,
        );
        match dispatcher.run().await {
            Ok(outputs) => {
                let _ = status_exec.send(ExecutionStatus::Completed(outputs));
            }
            Err(e) => {
                if let WorkflowError::SafeStopped {
                    last_completed_node,
                    interrupted_nodes,
                    checkpoint_saved,
                } = &e
                {
                    let _ = status_exec.send(ExecutionStatus::SafeStopped {
                        last_completed_node: last_completed_node.clone(),
                        interrupted_nodes: interrupted_nodes.clone(),
                        checkpoint_saved: *checkpoint_saved,
                    });
                    return;
                }

                handle_error_with_optional_handler(
                    schema.as_ref(),
                    &dispatcher,
                    &context,
                    &status_exec,
                    &error_event_emitter,
                    e,
                )
                .await;
            }
        }

        security_gate
            .record_workflow_end(context.as_ref(), workflow_id_for_end.as_deref())
            .await;

        #[cfg(feature = "plugin-system")]
        if let Some(registry) = plugin_registry_for_shutdown {
            let _ = registry.shutdown_all().await;
        }
    });

    let workflow_handle = WorkflowHandle::new(status_rx, events, event_active, command_tx);
    let debug_handle = DebugHandle::new(cmd_tx, debug_evt_rx);

    Ok((workflow_handle, debug_handle))
}

async fn handle_error_with_optional_handler<G, H>(
    schema: &WorkflowSchema,
    dispatcher: &WorkflowDispatcher<G, H>,
    context: &Arc<WorkflowContext>,
    status_exec: &watch::Sender<ExecutionStatus>,
    error_event_emitter: &EventEmitter,
    error: WorkflowError,
) where
    G: crate::core::debug::DebugGate,
    H: crate::core::debug::DebugHook,
{
    if let Some(error_handler) = &schema.error_handler {
        let partial_outputs = dispatcher.partial_outputs();
        let pool_snapshot = dispatcher.snapshot_pool().await;
        let error_context =
            crate::compiler::helpers::build_error_context(&error, schema, &partial_outputs);

        if error_event_emitter.is_active() {
            error_event_emitter
                .emit(GraphEngineEvent::ErrorHandlerStarted {
                    error: error.to_string(),
                })
                .await;
        }

        let runner = context
            .sub_graph_runner()
            .cloned()
            .unwrap_or_else(|| Arc::new(DefaultSubGraphRunner));
        match runner
            .run_sub_graph(
                &error_handler.sub_graph,
                &pool_snapshot,
                error_context,
                context.as_ref(),
            )
            .await
        {
            Ok(handler_outputs) => {
                let recovered_outputs: HashMap<String, Value> = handler_outputs
                    .as_object()
                    .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();

                if error_event_emitter.is_active() {
                    error_event_emitter
                        .emit(GraphEngineEvent::ErrorHandlerSucceeded {
                            outputs: recovered_outputs.clone(),
                        })
                        .await;
                }

                match error_handler.mode {
                    ErrorHandlingMode::Recover => {
                        let _ = status_exec.send(ExecutionStatus::FailedWithRecovery {
                            original_error: error.to_string(),
                            recovered_outputs,
                        });
                    }
                    ErrorHandlingMode::Notify => {
                        let _ = status_exec.send(ExecutionStatus::Failed(error.to_string()));
                    }
                }
            }
            Err(handler_err) => {
                if error_event_emitter.is_active() {
                    error_event_emitter
                        .emit(GraphEngineEvent::ErrorHandlerFailed {
                            error: handler_err.to_string(),
                        })
                        .await;
                }
                let _ = status_exec.send(ExecutionStatus::Failed(error.to_string()));
            }
        }
    } else {
        let _ = status_exec.send(ExecutionStatus::Failed(error.to_string()));
    }
}
