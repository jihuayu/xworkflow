use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::core::event_bus::GraphEngineEvent;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::NodeRunResult;
use crate::error::{WorkflowResult, WorkflowError};

/// Debug gate checks whether a pause is needed without async overhead.
pub trait DebugGate: Send + Sync {
    fn should_pause_before(&self, node_id: &str) -> bool;
    fn should_pause_after(&self, node_id: &str) -> bool;
}

/// Debug hook invoked only when gate requests a pause.
#[async_trait]
pub trait DebugHook: Send + Sync {
    async fn before_node_execute(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction>;

    async fn after_node_execute(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        result: &NodeRunResult,
        variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction>;
}

/// Debug action returned by hook.
#[derive(Debug, Clone)]
pub enum DebugAction {
    Continue,
    Abort { reason: String },
    UpdateVariables {
        variables: HashMap<String, Value>,
        then: Box<DebugAction>,
    },
    SkipNode,
}

#[derive(Debug, Clone, Default)]
pub struct DebugConfig {
    pub breakpoints: HashSet<String>,
    pub break_on_start: bool,
    pub auto_snapshot: bool,
}

#[derive(Debug, Clone)]
pub enum DebugCommand {
    Step,
    StepOver,
    Continue,
    Abort { reason: Option<String> },
    AddBreakpoint { node_id: String },
    RemoveBreakpoint { node_id: String },
    ClearBreakpoints,
    InspectVariables,
    InspectNodeVariables { node_id: String },
    UpdateVariables { variables: HashMap<String, Value> },
    QueryState,
}

#[derive(Debug, Clone)]
pub enum DebugEvent {
    Paused { reason: PauseReason, location: PauseLocation },
    Resumed,
    BreakpointAdded { node_id: String },
    BreakpointRemoved { node_id: String },
    VariableSnapshot { variables: HashMap<String, Segment> },
    NodeVariableSnapshot { node_id: String, variables: HashMap<String, Segment> },
    StateReport { state: DebugState, breakpoints: HashSet<String>, step_count: i32 },
    VariablesUpdated { updated_keys: Vec<String> },
    Error { message: String },
}

#[derive(Debug, Clone)]
pub enum DebugState {
    Running,
    Paused {
        location: PauseLocation,
        variable_snapshot: Option<HashMap<String, Segment>>,
    },
    Finished,
}

#[derive(Debug, Clone)]
pub enum PauseLocation {
    BeforeNode {
        node_id: String,
        node_type: String,
        node_title: String,
    },
    AfterNode {
        node_id: String,
        node_type: String,
        node_title: String,
        result: NodeRunResult,
    },
}

#[derive(Debug, Clone)]
pub enum PauseReason {
    Breakpoint,
    Step,
    UserRequested,
    Initial,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepMode {
    Run,
    Step,
    Initial,
}

/// No-op gate for zero-overhead mode.
pub struct NoopGate;

impl DebugGate for NoopGate {
    #[inline(always)]
    fn should_pause_before(&self, _node_id: &str) -> bool {
        false
    }

    #[inline(always)]
    fn should_pause_after(&self, _node_id: &str) -> bool {
        false
    }
}

/// No-op hook used with NoopGate.
pub struct NoopHook;

#[async_trait]
impl DebugHook for NoopHook {
    async fn before_node_execute(
        &self,
        _node_id: &str,
        _node_type: &str,
        _node_title: &str,
        _variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction> {
        Ok(DebugAction::Continue)
    }

    async fn after_node_execute(
        &self,
        _node_id: &str,
        _node_type: &str,
        _node_title: &str,
        _result: &NodeRunResult,
        _variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction> {
        Ok(DebugAction::Continue)
    }
}

pub struct InteractiveDebugGate {
    pub(crate) config: Arc<RwLock<DebugConfig>>,
    pub(crate) mode: Arc<RwLock<StepMode>>,
}

impl DebugGate for InteractiveDebugGate {
    fn should_pause_before(&self, node_id: &str) -> bool {
        let mode = self.mode.try_read();
        let config = self.config.try_read();

        match (mode, config) {
            (Ok(mode), Ok(config)) => {
                *mode == StepMode::Step
                    || (*mode == StepMode::Initial && config.break_on_start)
                    || config.breakpoints.contains(node_id)
            }
            _ => false,
        }
    }

    fn should_pause_after(&self, _node_id: &str) -> bool {
        self.mode
            .try_read()
            .map(|m| *m == StepMode::Step)
            .unwrap_or(false)
    }
}

pub struct InteractiveDebugHook {
    pub(crate) cmd_rx: Mutex<mpsc::Receiver<DebugCommand>>,
    pub(crate) event_tx: mpsc::Sender<DebugEvent>,
    pub(crate) graph_event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    pub(crate) config: Arc<RwLock<DebugConfig>>,
    pub(crate) mode: Arc<RwLock<StepMode>>,
    pub(crate) last_pause: Arc<RwLock<Option<PauseLocation>>>,
    pub(crate) step_count: Arc<RwLock<i32>>,
}

#[async_trait]
impl DebugHook for InteractiveDebugHook {
    async fn before_node_execute(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction> {
        let reason = {
            let config = self.config.read().await;
            let mode = self.mode.read().await;
            if config.breakpoints.contains(node_id) {
                PauseReason::Breakpoint
            } else if *mode == StepMode::Step {
                PauseReason::Step
            } else if *mode == StepMode::Initial {
                PauseReason::Initial
            } else {
                PauseReason::UserRequested
            }
        };

        let location = PauseLocation::BeforeNode {
            node_id: node_id.to_string(),
            node_type: node_type.to_string(),
            node_title: node_title.to_string(),
        };
        *self.last_pause.write().await = Some(location.clone());
        *self.step_count.write().await += 1;

        self.emit_paused(reason.clone(), location.clone(), "before").await;

        if self.config.read().await.auto_snapshot {
            self.emit_snapshot(variable_pool).await;
        }

        self.wait_for_command(variable_pool).await
    }

    async fn after_node_execute(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        result: &NodeRunResult,
        variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction> {
        let location = PauseLocation::AfterNode {
            node_id: node_id.to_string(),
            node_type: node_type.to_string(),
            node_title: node_title.to_string(),
            result: result.clone(),
        };
        *self.last_pause.write().await = Some(location.clone());
        *self.step_count.write().await += 1;

        self.emit_paused(PauseReason::Step, location, "after").await;
        self.wait_for_command(variable_pool).await
    }
}

impl InteractiveDebugHook {
    async fn emit_paused(&self, reason: PauseReason, location: PauseLocation, phase: &str) {
        let _ = self
            .event_tx
            .send(DebugEvent::Paused {
                reason: reason.clone(),
                location: location.clone(),
            })
            .await;

        if let Some(tx) = &self.graph_event_tx {
            let (node_id, node_type, node_title) = match &location {
                PauseLocation::BeforeNode { node_id, node_type, node_title } => {
                    (node_id.clone(), node_type.clone(), node_title.clone())
                }
                PauseLocation::AfterNode { node_id, node_type, node_title, .. } => {
                    (node_id.clone(), node_type.clone(), node_title.clone())
                }
            };
            let reason_str = match reason {
                PauseReason::Breakpoint => "breakpoint",
                PauseReason::Step => "step",
                PauseReason::Initial => "initial",
                PauseReason::UserRequested => "user_requested",
            };
            let _ = tx
                .send(GraphEngineEvent::DebugPaused {
                    reason: reason_str.to_string(),
                    node_id,
                    node_type,
                    node_title,
                    phase: phase.to_string(),
                })
                .await;
        }
    }

    async fn emit_resumed(&self) {
        let _ = self.event_tx.send(DebugEvent::Resumed).await;
        if let Some(tx) = &self.graph_event_tx {
            let _ = tx.send(GraphEngineEvent::DebugResumed).await;
        }
    }

    async fn emit_snapshot(&self, variable_pool: &VariablePool) {
        let snapshot = variable_pool.snapshot();
        let _ = self
            .event_tx
            .send(DebugEvent::VariableSnapshot {
                variables: snapshot.clone(),
            })
            .await;
        if let Some(tx) = &self.graph_event_tx {
            let data = serde_json::to_value(&snapshot).unwrap_or(Value::Null);
            let _ = tx.send(GraphEngineEvent::DebugVariableSnapshot { data }).await;
        }
    }

    async fn wait_for_command(&self, variable_pool: &VariablePool) -> WorkflowResult<DebugAction> {
        loop {
            let cmd = {
                let mut rx = self.cmd_rx.lock().await;
                rx.recv().await
            };

            match cmd {
                Some(DebugCommand::Step) | Some(DebugCommand::StepOver) => {
                    *self.mode.write().await = StepMode::Step;
                    self.emit_resumed().await;
                    return Ok(DebugAction::Continue);
                }
                Some(DebugCommand::Continue) => {
                    *self.mode.write().await = StepMode::Run;
                    self.emit_resumed().await;
                    return Ok(DebugAction::Continue);
                }
                Some(DebugCommand::Abort { reason }) => {
                    return Ok(DebugAction::Abort {
                        reason: reason.unwrap_or_else(|| "User aborted".into()),
                    });
                }
                Some(DebugCommand::AddBreakpoint { node_id }) => {
                    self.config.write().await.breakpoints.insert(node_id.clone());
                    let _ = self
                        .event_tx
                        .send(DebugEvent::BreakpointAdded { node_id: node_id.clone() })
                        .await;
                    if let Some(tx) = &self.graph_event_tx {
                        let _ = tx
                            .send(GraphEngineEvent::DebugBreakpointChanged {
                                action: "added".to_string(),
                                node_id: Some(node_id),
                            })
                            .await;
                    }
                }
                Some(DebugCommand::RemoveBreakpoint { node_id }) => {
                    self.config.write().await.breakpoints.remove(&node_id);
                    let _ = self
                        .event_tx
                        .send(DebugEvent::BreakpointRemoved { node_id: node_id.clone() })
                        .await;
                    if let Some(tx) = &self.graph_event_tx {
                        let _ = tx
                            .send(GraphEngineEvent::DebugBreakpointChanged {
                                action: "removed".to_string(),
                                node_id: Some(node_id),
                            })
                            .await;
                    }
                }
                Some(DebugCommand::ClearBreakpoints) => {
                    self.config.write().await.breakpoints.clear();
                    if let Some(tx) = &self.graph_event_tx {
                        let _ = tx
                            .send(GraphEngineEvent::DebugBreakpointChanged {
                                action: "cleared".to_string(),
                                node_id: None,
                            })
                            .await;
                    }
                }
                Some(DebugCommand::InspectVariables) => {
                    self.emit_snapshot(variable_pool).await;
                }
                Some(DebugCommand::InspectNodeVariables { node_id }) => {
                    let vars = variable_pool.get_node_variables(&node_id);
                    let _ = self
                        .event_tx
                        .send(DebugEvent::NodeVariableSnapshot { node_id, variables: vars })
                        .await;
                }
                Some(DebugCommand::UpdateVariables { variables }) => {
                    let keys: Vec<String> = variables.keys().cloned().collect();
                    let _ = self
                        .event_tx
                        .send(DebugEvent::VariablesUpdated { updated_keys: keys })
                        .await;
                    return Ok(DebugAction::UpdateVariables {
                        variables,
                        then: Box::new(DebugAction::Continue),
                    });
                }
                Some(DebugCommand::QueryState) => {
                    let config = self.config.read().await;
                    let last = self.last_pause.read().await.clone();
                    let state = match last {
                        Some(location) => DebugState::Paused { location, variable_snapshot: None },
                        None => DebugState::Running,
                    };
                    let step_count = *self.step_count.read().await;
                    let _ = self
                        .event_tx
                        .send(DebugEvent::StateReport {
                            state,
                            breakpoints: config.breakpoints.clone(),
                            step_count,
                        })
                        .await;
                }
                None => {
                    return Err(WorkflowError::Aborted("Debug channel closed".into()));
                }
            }
        }
    }
}

pub struct DebugHandle {
    cmd_tx: mpsc::Sender<DebugCommand>,
    event_rx: Mutex<mpsc::Receiver<DebugEvent>>,
}

impl DebugHandle {
    pub async fn step(&self) -> Result<(), DebugError> {
        self.cmd_tx
            .send(DebugCommand::Step)
            .await
            .map_err(|_| DebugError::ChannelClosed)
    }

    pub async fn continue_run(&self) -> Result<(), DebugError> {
        self.cmd_tx
            .send(DebugCommand::Continue)
            .await
            .map_err(|_| DebugError::ChannelClosed)
    }

    pub async fn abort(&self, reason: Option<String>) -> Result<(), DebugError> {
        self.cmd_tx
            .send(DebugCommand::Abort { reason })
            .await
            .map_err(|_| DebugError::ChannelClosed)
    }

    pub async fn add_breakpoint(&self, node_id: &str) -> Result<(), DebugError> {
        self.cmd_tx
            .send(DebugCommand::AddBreakpoint {
                node_id: node_id.to_string(),
            })
            .await
            .map_err(|_| DebugError::ChannelClosed)
    }

    pub async fn remove_breakpoint(&self, node_id: &str) -> Result<(), DebugError> {
        self.cmd_tx
            .send(DebugCommand::RemoveBreakpoint {
                node_id: node_id.to_string(),
            })
            .await
            .map_err(|_| DebugError::ChannelClosed)
    }

    pub async fn inspect_variables(&self) -> Result<(), DebugError> {
        self.cmd_tx
            .send(DebugCommand::InspectVariables)
            .await
            .map_err(|_| DebugError::ChannelClosed)
    }

    pub async fn inspect_node(&self, node_id: &str) -> Result<(), DebugError> {
        self.cmd_tx
            .send(DebugCommand::InspectNodeVariables {
                node_id: node_id.to_string(),
            })
            .await
            .map_err(|_| DebugError::ChannelClosed)
    }

    pub async fn update_variables(
        &self,
        variables: HashMap<String, Value>,
    ) -> Result<(), DebugError> {
        self.cmd_tx
            .send(DebugCommand::UpdateVariables { variables })
            .await
            .map_err(|_| DebugError::ChannelClosed)
    }

    pub async fn next_event(&self) -> Option<DebugEvent> {
        self.event_rx.lock().await.recv().await
    }

    pub async fn wait_for_pause(&self) -> Result<DebugEvent, DebugError> {
        loop {
            match self.next_event().await {
                Some(evt @ DebugEvent::Paused { .. }) => return Ok(evt),
                Some(_) => continue,
                None => return Err(DebugError::ChannelClosed),
            }
        }
    }

    pub(crate) fn new(cmd_tx: mpsc::Sender<DebugCommand>, event_rx: mpsc::Receiver<DebugEvent>) -> Self {
        Self {
            cmd_tx,
            event_rx: Mutex::new(event_rx),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DebugError {
    #[error("Debug channel closed")]
    ChannelClosed,
    #[error("Debug timeout")]
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noop_gate_always_false() {
        let gate = NoopGate;
        assert!(!gate.should_pause_before("n1"));
        assert!(!gate.should_pause_after("n1"));
    }

    #[tokio::test]
    async fn test_interactive_gate_breakpoint() {
        let mut cfg = DebugConfig::default();
        cfg.breakpoints.insert("n1".to_string());
        let config = Arc::new(RwLock::new(cfg));
        let mode = Arc::new(RwLock::new(StepMode::Run));
        let gate = InteractiveDebugGate { config, mode };
        assert!(gate.should_pause_before("n1"));
        assert!(!gate.should_pause_before("n2"));
    }

    #[tokio::test]
    async fn test_interactive_gate_step_mode() {
        let config = Arc::new(RwLock::new(DebugConfig::default()));
        let mode = Arc::new(RwLock::new(StepMode::Step));
        let gate = InteractiveDebugGate { config, mode };
        assert!(gate.should_pause_before("any"));
        assert!(gate.should_pause_after("any"));
    }

    #[tokio::test]
    async fn test_command_loop_step() {
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let (evt_tx, _evt_rx) = mpsc::channel(4);
        let config = Arc::new(RwLock::new(DebugConfig::default()));
        let mode = Arc::new(RwLock::new(StepMode::Run));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config: config.clone(),
            mode: mode.clone(),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };

        let pool = VariablePool::new();
        cmd_tx.send(DebugCommand::Step).await.unwrap();
        let action = hook.wait_for_command(&pool).await.unwrap();
        assert!(matches!(action, DebugAction::Continue));
        assert_eq!(*mode.read().await, StepMode::Step);
    }

    #[tokio::test]
    async fn test_command_loop_breakpoint_mgmt() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (evt_tx, _evt_rx) = mpsc::channel(8);
        let config = Arc::new(RwLock::new(DebugConfig::default()));
        let mode = Arc::new(RwLock::new(StepMode::Run));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config: config.clone(),
            mode,
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };

        let pool = VariablePool::new();
        cmd_tx
            .send(DebugCommand::AddBreakpoint { node_id: "n1".into() })
            .await
            .unwrap();
        cmd_tx
            .send(DebugCommand::RemoveBreakpoint { node_id: "n1".into() })
            .await
            .unwrap();
        cmd_tx.send(DebugCommand::Continue).await.unwrap();

        let action = hook.wait_for_command(&pool).await.unwrap();
        assert!(matches!(action, DebugAction::Continue));
        assert!(config.read().await.breakpoints.is_empty());
    }
}
