//! Workflow handle shared by schema-run and compiled-run entry points.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};

use crate::core::dispatcher::Command;
use crate::core::event_bus::GraphEngineEvent;
use crate::domain::execution::ExecutionStatus;
use crate::dsl::schema::HumanInputDecision;
use crate::error::WorkflowError;

/// Handle to a running or completed workflow.
///
/// Allows polling [`status()`](Self::status), blocking on completion via
/// [`wait()`](Self::wait), and retrieving collected engine events.
pub struct WorkflowHandle {
    status_rx: watch::Receiver<ExecutionStatus>,
    events: Option<Arc<Mutex<Vec<GraphEngineEvent>>>>,
    event_active: Arc<AtomicBool>,
    command_tx: mpsc::Sender<Command>,
}

impl WorkflowHandle {
    pub(crate) fn new(
        status_rx: watch::Receiver<ExecutionStatus>,
        events: Option<Arc<Mutex<Vec<GraphEngineEvent>>>>,
        event_active: Arc<AtomicBool>,
        command_tx: mpsc::Sender<Command>,
    ) -> Self {
        Self {
            status_rx,
            events,
            event_active,
            command_tx,
        }
    }

    /// Return the current execution status (non-blocking).
    pub async fn status(&self) -> ExecutionStatus {
        self.status_rx.borrow().clone()
    }

    /// Return a snapshot of all collected engine events so far.
    pub async fn events(&self) -> Vec<GraphEngineEvent> {
        match &self.events {
            Some(events) => events.lock().await.clone(),
            None => Vec::new(),
        }
    }

    /// Block until the workflow reaches a terminal status.
    pub async fn wait(&self) -> ExecutionStatus {
        let mut rx = self.status_rx.clone();
        loop {
            let status = rx.borrow().clone();
            match status {
                ExecutionStatus::Running => {
                    if rx.changed().await.is_err() {
                        return rx.borrow().clone();
                    }
                }
                _ => return status,
            }
        }
    }

    pub async fn wait_or_paused(&self) -> ExecutionStatus {
        self.wait().await
    }

    pub async fn resume_with_input(
        &self,
        input: HashMap<String, Value>,
    ) -> Result<(), WorkflowError> {
        self.command_tx
            .send(Command::ResumeWithInput { input })
            .await
            .map_err(|_| WorkflowError::InternalError("Workflow already terminated".to_string()))
    }

    pub async fn resume_human_input(
        &self,
        node_id: &str,
        resume_token: &str,
        decision: Option<HumanInputDecision>,
        form_data: HashMap<String, Value>,
    ) -> Result<(), WorkflowError> {
        self.command_tx
            .send(Command::ResumeHumanInput {
                node_id: node_id.to_string(),
                resume_token: resume_token.to_string(),
                decision,
                form_data,
            })
            .await
            .map_err(|_| WorkflowError::InternalError("Workflow already terminated".to_string()))
    }

    pub async fn safe_stop(&self) -> Result<(), WorkflowError> {
        self.command_tx
            .send(Command::SafeStop)
            .await
            .map_err(|_| WorkflowError::InternalError("Workflow already terminated".to_string()))
    }

    /// Whether event collection is still active.
    pub fn events_active(&self) -> bool {
        self.event_active.load(Ordering::Relaxed)
    }
}
