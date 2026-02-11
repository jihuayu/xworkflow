//! Interactive debugger for workflow execution.
//!
//! The debug system uses a two-layer design:
//! - **[`DebugGate`]**: A cheap, synchronous check that decides whether to pause
//!   before/after a node. The no-op gate ([`NoopGate`]) is fully inlined away.
//! - **[`DebugHook`]**: An async callback invoked only when the gate requests a
//!   pause. The hook can inspect variables, update them, or abort execution.
//!
//! For interactive debugging, use [`InteractiveDebugGate`] and
//! [`InteractiveDebugHook`] with a [`DebugHandle`] for sending commands.

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

/// Configuration for the interactive debugger.
#[derive(Debug, Clone, Default)]
pub struct DebugConfig {
    /// Set of node IDs where execution should pause.
    pub breakpoints: HashSet<String>,
    /// Whether to pause before the first node executes.
    pub break_on_start: bool,
    /// Whether to auto-capture variable snapshots on pause.
    pub auto_snapshot: bool,
}

/// Command sent to the debugger via the [`DebugHandle`].
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

/// Event emitted by the debugger to the external controller.
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

/// Current state of the debugger.
#[derive(Debug, Clone)]
pub enum DebugState {
    Running,
    Paused {
        location: PauseLocation,
        variable_snapshot: Option<HashMap<String, Segment>>,
    },
    Finished,
}

/// Location in the graph where execution is paused.
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

/// Reason the debugger paused execution.
#[derive(Debug, Clone)]
pub enum PauseReason {
    Breakpoint,
    Step,
    UserRequested,
    Initial,
}

/// Internal stepping mode for the debugger.
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

/// Interactive [`DebugGate`] backed by a shared config and step mode.
///
/// This gate checks breakpoints and step mode to decide whether to pause.
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

/// Interactive [`DebugHook`] that waits for [`DebugCommand`]s from an
/// external controller and emits [`DebugEvent`]s back.
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

    #[tokio::test]
    async fn test_command_loop_abort() {
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let (evt_tx, _evt_rx) = mpsc::channel(4);
        let config = Arc::new(RwLock::new(DebugConfig::default()));
        let mode = Arc::new(RwLock::new(StepMode::Run));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config,
            mode,
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        cmd_tx
            .send(DebugCommand::Abort { reason: Some("user quit".into()) })
            .await
            .unwrap();
        let action = hook.wait_for_command(&pool).await.unwrap();
        match action {
            DebugAction::Abort { reason } => assert_eq!(reason, "user quit"),
            other => panic!("Expected Abort, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_command_loop_abort_no_reason() {
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let (evt_tx, _evt_rx) = mpsc::channel(4);
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config: Arc::new(RwLock::new(DebugConfig::default())),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        cmd_tx
            .send(DebugCommand::Abort { reason: None })
            .await
            .unwrap();
        let action = hook.wait_for_command(&pool).await.unwrap();
        match action {
            DebugAction::Abort { reason } => assert_eq!(reason, "User aborted"),
            other => panic!("Expected Abort, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_command_loop_update_variables() {
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let (evt_tx, mut evt_rx) = mpsc::channel(4);
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config: Arc::new(RwLock::new(DebugConfig::default())),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        let mut vars = HashMap::new();
        vars.insert("n1.x".to_string(), Value::Number(42.into()));
        cmd_tx
            .send(DebugCommand::UpdateVariables { variables: vars })
            .await
            .unwrap();
        let action = hook.wait_for_command(&pool).await.unwrap();
        assert!(matches!(action, DebugAction::UpdateVariables { .. }));
        // Check event emitted
        let evt = evt_rx.recv().await.unwrap();
        assert!(matches!(evt, DebugEvent::VariablesUpdated { .. }));
    }

    #[tokio::test]
    async fn test_command_loop_inspect_node() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (evt_tx, mut evt_rx) = mpsc::channel(8);
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config: Arc::new(RwLock::new(DebugConfig::default())),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("n1", "x"),
            Segment::Integer(10),
        );
        cmd_tx
            .send(DebugCommand::InspectNodeVariables { node_id: "n1".into() })
            .await
            .unwrap();
        cmd_tx.send(DebugCommand::Continue).await.unwrap();
        let _ = hook.wait_for_command(&pool).await.unwrap();
        let evt = evt_rx.recv().await.unwrap();
        match evt {
            DebugEvent::NodeVariableSnapshot { node_id, variables } => {
                assert_eq!(node_id, "n1");
                assert!(!variables.is_empty());
            }
            other => panic!("Expected NodeVariableSnapshot, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_command_loop_query_state() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (evt_tx, mut evt_rx) = mpsc::channel(8);
        let mut cfg = DebugConfig::default();
        cfg.breakpoints.insert("bp1".into());
        let config = Arc::new(RwLock::new(cfg));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config,
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        cmd_tx.send(DebugCommand::QueryState).await.unwrap();
        cmd_tx.send(DebugCommand::Continue).await.unwrap();
        let _ = hook.wait_for_command(&pool).await.unwrap();
        let evt = evt_rx.recv().await.unwrap();
        match evt {
            DebugEvent::StateReport { state, breakpoints, step_count } => {
                assert!(matches!(state, DebugState::Running));
                assert!(breakpoints.contains("bp1"));
                assert_eq!(step_count, 0);
            }
            other => panic!("Expected StateReport, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_command_loop_clear_breakpoints() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (evt_tx, _evt_rx) = mpsc::channel(8);
        let mut cfg = DebugConfig::default();
        cfg.breakpoints.insert("a".into());
        cfg.breakpoints.insert("b".into());
        let config = Arc::new(RwLock::new(cfg));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config: config.clone(),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        cmd_tx.send(DebugCommand::ClearBreakpoints).await.unwrap();
        cmd_tx.send(DebugCommand::Continue).await.unwrap();
        let _ = hook.wait_for_command(&pool).await.unwrap();
        assert!(config.read().await.breakpoints.is_empty());
    }

    #[tokio::test]
    async fn test_command_loop_step_over() {
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let (evt_tx, _evt_rx) = mpsc::channel(4);
        let mode = Arc::new(RwLock::new(StepMode::Run));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config: Arc::new(RwLock::new(DebugConfig::default())),
            mode: mode.clone(),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        cmd_tx.send(DebugCommand::StepOver).await.unwrap();
        let action = hook.wait_for_command(&pool).await.unwrap();
        assert!(matches!(action, DebugAction::Continue));
        assert_eq!(*mode.read().await, StepMode::Step);
    }

    #[tokio::test]
    async fn test_command_channel_closed() {
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let (evt_tx, _evt_rx) = mpsc::channel(4);
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config: Arc::new(RwLock::new(DebugConfig::default())),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        drop(cmd_tx);
        let result = hook.wait_for_command(&pool).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_interactive_gate_initial_break_on_start() {
        let mut cfg = DebugConfig::default();
        cfg.break_on_start = true;
        let config = Arc::new(RwLock::new(cfg));
        let mode = Arc::new(RwLock::new(StepMode::Initial));
        let gate = InteractiveDebugGate { config, mode };
        assert!(gate.should_pause_before("start"));
        assert!(!gate.should_pause_after("start"));
    }

    #[tokio::test]
    async fn test_noop_hook() {
        let hook = NoopHook;
        let pool = VariablePool::new();
        let result = NodeRunResult::default();
        let action = hook.before_node_execute("n1", "start", "Start", &pool).await.unwrap();
        assert!(matches!(action, DebugAction::Continue));
        let action2 = hook.after_node_execute("n1", "start", "Start", &result, &pool).await.unwrap();
        assert!(matches!(action2, DebugAction::Continue));
    }

    #[test]
    fn test_debug_error_display() {
        let e1 = DebugError::ChannelClosed;
        assert_eq!(e1.to_string(), "Debug channel closed");
        let e2 = DebugError::Timeout;
        assert_eq!(e2.to_string(), "Debug timeout");
    }

    #[test]
    fn test_debug_config_default() {
        let cfg = DebugConfig::default();
        assert!(!cfg.break_on_start);
        assert!(!cfg.auto_snapshot);
        assert!(cfg.breakpoints.is_empty());
    }

    #[test]
    fn test_debug_action_debug() {
        let action = DebugAction::Continue;
        let debug_str = format!("{:?}", action);
        assert!(debug_str.contains("Continue"));

        let action2 = DebugAction::SkipNode;
        let debug_str2 = format!("{:?}", action2);
        assert!(debug_str2.contains("SkipNode"));
    }

    #[test]
    fn test_pause_reason_debug() {
        let r = PauseReason::Breakpoint;
        assert!(format!("{:?}", r).contains("Breakpoint"));
    }

    #[test]
    fn test_debug_state_debug() {
        let s = DebugState::Running;
        assert!(format!("{:?}", s).contains("Running"));
        let s2 = DebugState::Finished;
        assert!(format!("{:?}", s2).contains("Finished"));
    }

    #[tokio::test]
    async fn test_emit_paused_with_graph_event_tx() {
        let (evt_tx, mut evt_rx) = mpsc::channel(8);
        let (graph_tx, mut graph_rx) = mpsc::channel(8);
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(mpsc::channel(1).1),
            event_tx: evt_tx,
            graph_event_tx: Some(graph_tx),
            config: Arc::new(RwLock::new(DebugConfig::default())),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let location = PauseLocation::BeforeNode {
            node_id: "n1".into(),
            node_type: "start".into(),
            node_title: "Start".into(),
        };
        hook.emit_paused(PauseReason::Breakpoint, location, "before").await;

        let evt = evt_rx.recv().await.unwrap();
        assert!(matches!(evt, DebugEvent::Paused { .. }));
        let graph_evt = graph_rx.recv().await.unwrap();
        assert!(matches!(graph_evt, GraphEngineEvent::DebugPaused { .. }));
    }

    #[tokio::test]
    async fn test_emit_paused_after_node() {
        let (evt_tx, mut evt_rx) = mpsc::channel(8);
        let (graph_tx, mut graph_rx) = mpsc::channel(8);
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(mpsc::channel(1).1),
            event_tx: evt_tx,
            graph_event_tx: Some(graph_tx),
            config: Arc::new(RwLock::new(DebugConfig::default())),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let result = NodeRunResult::default();
        let location = PauseLocation::AfterNode {
            node_id: "n1".into(),
            node_type: "code".into(),
            node_title: "Code".into(),
            result,
        };
        hook.emit_paused(PauseReason::Step, location, "after").await;

        let evt = evt_rx.recv().await.unwrap();
        assert!(matches!(evt, DebugEvent::Paused { .. }));
        let graph_evt = graph_rx.recv().await.unwrap();
        match graph_evt {
            GraphEngineEvent::DebugPaused { phase, reason, .. } => {
                assert_eq!(phase, "after");
                assert_eq!(reason, "step");
            }
            other => panic!("Expected DebugPaused, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_emit_resumed_with_graph_event_tx() {
        let (evt_tx, mut evt_rx) = mpsc::channel(4);
        let (graph_tx, mut graph_rx) = mpsc::channel(4);
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(mpsc::channel(1).1),
            event_tx: evt_tx,
            graph_event_tx: Some(graph_tx),
            config: Arc::new(RwLock::new(DebugConfig::default())),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        hook.emit_resumed().await;
        let evt = evt_rx.recv().await.unwrap();
        assert!(matches!(evt, DebugEvent::Resumed));
        let graph_evt = graph_rx.recv().await.unwrap();
        assert!(matches!(graph_evt, GraphEngineEvent::DebugResumed));
    }

    #[tokio::test]
    async fn test_emit_snapshot_with_graph_event_tx() {
        let (evt_tx, mut evt_rx) = mpsc::channel(4);
        let (graph_tx, mut graph_rx) = mpsc::channel(4);
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(mpsc::channel(1).1),
            event_tx: evt_tx,
            graph_event_tx: Some(graph_tx),
            config: Arc::new(RwLock::new(DebugConfig::default())),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("n1", "x"),
            Segment::Integer(5),
        );
        hook.emit_snapshot(&pool).await;

        let evt = evt_rx.recv().await.unwrap();
        assert!(matches!(evt, DebugEvent::VariableSnapshot { .. }));
        let graph_evt = graph_rx.recv().await.unwrap();
        assert!(matches!(graph_evt, GraphEngineEvent::DebugVariableSnapshot { .. }));
    }

    #[tokio::test]
    async fn test_command_add_breakpoint_with_graph_tx() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (evt_tx, mut evt_rx) = mpsc::channel(8);
        let (graph_tx, mut graph_rx) = mpsc::channel(8);
        let config = Arc::new(RwLock::new(DebugConfig::default()));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: Some(graph_tx),
            config: config.clone(),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        cmd_tx
            .send(DebugCommand::AddBreakpoint { node_id: "bp1".into() })
            .await
            .unwrap();
        cmd_tx.send(DebugCommand::Continue).await.unwrap();
        let _ = hook.wait_for_command(&pool).await.unwrap();

        let evt = evt_rx.recv().await.unwrap();
        assert!(matches!(evt, DebugEvent::BreakpointAdded { .. }));
        let graph_evt = graph_rx.recv().await.unwrap();
        match graph_evt {
            GraphEngineEvent::DebugBreakpointChanged { action, node_id } => {
                assert_eq!(action, "added");
                assert_eq!(node_id.unwrap(), "bp1");
            }
            other => panic!("Expected DebugBreakpointChanged, got {:?}", other),
        }
        assert!(config.read().await.breakpoints.contains("bp1"));
    }

    #[tokio::test]
    async fn test_command_remove_breakpoint_with_graph_tx() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (evt_tx, mut evt_rx) = mpsc::channel(8);
        let (graph_tx, mut graph_rx) = mpsc::channel(8);
        let mut cfg = DebugConfig::default();
        cfg.breakpoints.insert("bp1".into());
        let config = Arc::new(RwLock::new(cfg));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: Some(graph_tx),
            config: config.clone(),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        cmd_tx
            .send(DebugCommand::RemoveBreakpoint { node_id: "bp1".into() })
            .await
            .unwrap();
        cmd_tx.send(DebugCommand::Continue).await.unwrap();
        let _ = hook.wait_for_command(&pool).await.unwrap();

        let evt = evt_rx.recv().await.unwrap();
        assert!(matches!(evt, DebugEvent::BreakpointRemoved { .. }));
        let graph_evt = graph_rx.recv().await.unwrap();
        match graph_evt {
            GraphEngineEvent::DebugBreakpointChanged { action, .. } => {
                assert_eq!(action, "removed");
            }
            other => panic!("Expected DebugBreakpointChanged, got {:?}", other),
        }
        assert!(!config.read().await.breakpoints.contains("bp1"));
    }

    #[tokio::test]
    async fn test_command_clear_breakpoints_with_graph_tx() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (evt_tx, _evt_rx) = mpsc::channel(8);
        let (graph_tx, mut graph_rx) = mpsc::channel(8);
        let mut cfg = DebugConfig::default();
        cfg.breakpoints.insert("a".into());
        let config = Arc::new(RwLock::new(cfg));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: Some(graph_tx),
            config: config.clone(),
            mode: Arc::new(RwLock::new(StepMode::Run)),
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };
        let pool = VariablePool::new();
        cmd_tx.send(DebugCommand::ClearBreakpoints).await.unwrap();
        cmd_tx.send(DebugCommand::Continue).await.unwrap();
        let _ = hook.wait_for_command(&pool).await.unwrap();

        let graph_evt = graph_rx.recv().await.unwrap();
        match graph_evt {
            GraphEngineEvent::DebugBreakpointChanged { action, node_id } => {
                assert_eq!(action, "cleared");
                assert!(node_id.is_none());
            }
            other => panic!("Expected DebugBreakpointChanged, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_query_state_with_paused_location() {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (evt_tx, mut evt_rx) = mpsc::channel(8);
        let config = Arc::new(RwLock::new(DebugConfig::default()));
        let last_pause = Arc::new(RwLock::new(Some(PauseLocation::BeforeNode {
            node_id: "n1".into(),
            node_type: "code".into(),
            node_title: "Code".into(),
        })));
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: evt_tx,
            graph_event_tx: None,
            config,
            mode: Arc::new(RwLock::new(StepMode::Step)),
            last_pause,
            step_count: Arc::new(RwLock::new(5)),
        };
        let pool = VariablePool::new();
        cmd_tx.send(DebugCommand::QueryState).await.unwrap();
        cmd_tx.send(DebugCommand::Continue).await.unwrap();
        let _ = hook.wait_for_command(&pool).await.unwrap();

        let evt = evt_rx.recv().await.unwrap();
        match evt {
            DebugEvent::StateReport { state, step_count, .. } => {
                assert!(matches!(state, DebugState::Paused { .. }));
                assert_eq!(step_count, 5);
            }
            other => panic!("Expected StateReport, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_debug_handle_methods() {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
        let (_evt_tx, evt_rx) = mpsc::channel(16);
        let handle = DebugHandle::new(cmd_tx, evt_rx);

        handle.step().await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, DebugCommand::Step));

        handle.continue_run().await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, DebugCommand::Continue));

        handle.abort(Some("bye".into())).await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, DebugCommand::Abort { reason: Some(_) }));

        handle.add_breakpoint("n1").await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, DebugCommand::AddBreakpoint { .. }));

        handle.remove_breakpoint("n1").await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, DebugCommand::RemoveBreakpoint { .. }));

        handle.inspect_variables().await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, DebugCommand::InspectVariables));

        handle.inspect_node("n1").await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, DebugCommand::InspectNodeVariables { .. }));

        let mut vars = HashMap::new();
        vars.insert("n.x".into(), Value::Bool(true));
        handle.update_variables(vars).await.unwrap();
        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, DebugCommand::UpdateVariables { .. }));
    }

    #[tokio::test]
    async fn test_debug_handle_channel_closed() {
        let (cmd_tx, _cmd_rx) = mpsc::channel(4);
        let (_evt_tx, evt_rx) = mpsc::channel(4);
        let handle = DebugHandle::new(cmd_tx, evt_rx);
        drop(_cmd_rx);
        let result = handle.step().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_debug_handle_wait_for_pause_closed() {
        let (_cmd_tx, _cmd_rx) = mpsc::channel(4);
        let (evt_tx, evt_rx) = mpsc::channel(4);
        let handle = DebugHandle::new(_cmd_tx, evt_rx);
        drop(evt_tx);
        let result = handle.wait_for_pause().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_debug_handle_next_event() {
        let (_cmd_tx, _cmd_rx) = mpsc::channel(4);
        let (evt_tx, evt_rx) = mpsc::channel(4);
        let handle = DebugHandle::new(_cmd_tx, evt_rx);
        evt_tx.send(DebugEvent::Resumed).await.unwrap();
        let evt = handle.next_event().await;
        assert!(matches!(evt, Some(DebugEvent::Resumed)));
    }
}
