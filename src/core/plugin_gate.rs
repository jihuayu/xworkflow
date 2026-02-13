//! Plugin gate trait for lifecycle hook emission.
//!
//! The [`PluginGate`] is invoked by the dispatcher at workflow/node boundaries
//! to run registered plugin hooks (before/after workflow, before/after node,
//! variable write interception).

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::dispatcher::EventEmitter;
use crate::core::variable_pool::{Segment, Selector, VariablePool};
use crate::dsl::schema::NodeRunResult;
use crate::error::WorkflowResult;

/// Gate for emitting plugin lifecycle hooks during workflow execution.
#[async_trait]
pub trait PluginGate: Send + Sync {
    /// Emit hooks before the workflow starts.
    async fn emit_before_workflow_hooks(&self) -> WorkflowResult<()>;

    async fn emit_after_workflow_hooks(
        &self,
        outputs: &HashMap<String, Value>,
        exceptions_count: i32,
    ) -> WorkflowResult<()>;

    async fn emit_before_node_hooks(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        node_config: &Value,
    ) -> WorkflowResult<()>;

    async fn emit_after_node_hooks(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        result: &NodeRunResult,
    ) -> WorkflowResult<()>;

    async fn apply_before_variable_write_hooks(
        &self,
        node_id: &str,
        selector: &Selector,
        value: &mut Segment,
    ) -> WorkflowResult<()>;

    fn mark_pool_dirty(&self);
}

/// No-op [`PluginGate`] used when no plugins are loaded.
#[derive(Debug, Default)]
pub struct NoopPluginGate;

#[async_trait]
impl PluginGate for NoopPluginGate {
    async fn emit_before_workflow_hooks(&self) -> WorkflowResult<()> {
        Ok(())
    }

    async fn emit_after_workflow_hooks(
        &self,
        _outputs: &HashMap<String, Value>,
        _exceptions_count: i32,
    ) -> WorkflowResult<()> {
        Ok(())
    }

    async fn emit_before_node_hooks(
        &self,
        _node_id: &str,
        _node_type: &str,
        _node_title: &str,
        _node_config: &Value,
    ) -> WorkflowResult<()> {
        Ok(())
    }

    async fn emit_after_node_hooks(
        &self,
        _node_id: &str,
        _node_type: &str,
        _node_title: &str,
        _result: &NodeRunResult,
    ) -> WorkflowResult<()> {
        Ok(())
    }

    async fn apply_before_variable_write_hooks(
        &self,
        _node_id: &str,
        _selector: &Selector,
        _value: &mut Segment,
    ) -> WorkflowResult<()> {
        Ok(())
    }

    fn mark_pool_dirty(&self) {}
}

#[cfg(feature = "plugin-system")]
mod real {
    use super::*;
    use arc_swap::ArcSwapOption;
    use parking_lot::RwLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::core::event_bus::GraphEngineEvent;
    use crate::plugin_system::{HookPayload, HookPoint, PluginRegistry};

    struct PoolSnapshotCache {
        snapshot: ArcSwapOption<VariablePool>,
        dirty: AtomicBool,
    }

    impl PoolSnapshotCache {
        fn new(initial: &VariablePool) -> Self {
            Self {
                snapshot: ArcSwapOption::new(Some(Arc::new(initial.clone()))),
                dirty: AtomicBool::new(false),
            }
        }

        fn mark_dirty(&self) {
            self.dirty.store(true, Ordering::Release);
        }

        fn get_or_refresh(&self, variable_pool: &Arc<RwLock<VariablePool>>) -> Arc<VariablePool> {
            if self.dirty.swap(false, Ordering::AcqRel) {
                let pool = variable_pool.read().clone();
                self.snapshot.store(Some(Arc::new(pool)));
            }
            self.snapshot
                .load_full()
                .expect("pool snapshot should be set")
        }
    }

    pub struct RealPluginGate {
        variable_pool: Arc<RwLock<VariablePool>>,
        event_emitter: EventEmitter,
        plugin_registry: Option<Arc<PluginRegistry>>,
        cache: PoolSnapshotCache,
    }

    impl RealPluginGate {
        pub fn new(
            variable_pool: Arc<RwLock<VariablePool>>,
            event_emitter: EventEmitter,
            plugin_registry: Option<Arc<PluginRegistry>>,
        ) -> Self {
            let initial = variable_pool.read().clone();
            Self {
                variable_pool,
                event_emitter,
                plugin_registry,
                cache: PoolSnapshotCache::new(&initial),
            }
        }

        fn pool_snapshot(&self) -> Arc<VariablePool> {
            self.cache.get_or_refresh(&self.variable_pool)
        }

        async fn execute_hooks<F>(
            &self,
            hook_point: HookPoint,
            payload_fn: F,
        ) -> WorkflowResult<Vec<Value>>
        where
            F: FnOnce() -> Value,
        {
            let mut results = Vec::new();
            if let Some(reg) = &self.plugin_registry {
                let mut handlers = reg.hooks(&hook_point);
                if handlers.is_empty() {
                    return Ok(results);
                }

                handlers.sort_by_key(|h| h.priority());
                let data = payload_fn();
                let pool_snapshot = self.pool_snapshot();
                let payload = HookPayload {
                    hook_point,
                    data,
                    variable_pool: Some(pool_snapshot),
                    event_tx: Some(self.event_emitter.tx().clone()),
                };

                for handler in handlers {
                    match handler.handle(&payload).await {
                        Ok(Some(value)) => results.push(value),
                        Ok(None) => {}
                        Err(e) => {
                            self.event_emitter
                                .emit(GraphEngineEvent::PluginError {
                                    plugin_id: handler.name().to_string(),
                                    error: e.to_string(),
                                })
                                .await;
                        }
                    }
                }
            }
            Ok(results)
        }
    }

    #[async_trait]
    impl PluginGate for RealPluginGate {
        async fn emit_before_workflow_hooks(&self) -> WorkflowResult<()> {
            let _ = self
                .execute_hooks(HookPoint::BeforeWorkflowRun, || {
                    serde_json::json!({
                      "event": "before_workflow_run",
                    })
                })
                .await?;
            Ok(())
        }

        async fn emit_after_workflow_hooks(
            &self,
            outputs: &HashMap<String, Value>,
            exceptions_count: i32,
        ) -> WorkflowResult<()> {
            let outputs = outputs.clone();
            let _ = self
                .execute_hooks(HookPoint::AfterWorkflowRun, || {
                    serde_json::json!({
                      "event": "after_workflow_run",
                      "outputs": outputs,
                      "exceptions_count": exceptions_count,
                    })
                })
                .await?;
            Ok(())
        }

        async fn emit_before_node_hooks(
            &self,
            node_id: &str,
            node_type: &str,
            node_title: &str,
            node_config: &Value,
        ) -> WorkflowResult<()> {
            let node_config = node_config.clone();
            let _ = self
                .execute_hooks(HookPoint::BeforeNodeExecute, || {
                    serde_json::json!({
                      "event": "before_node_execute",
                      "node_id": node_id,
                      "node_type": node_type,
                      "node_title": node_title,
                      "config": node_config,
                    })
                })
                .await?;
            Ok(())
        }

        async fn emit_after_node_hooks(
            &self,
            node_id: &str,
            node_type: &str,
            node_title: &str,
            result: &NodeRunResult,
        ) -> WorkflowResult<()> {
            let status = format!("{:?}", result.status);
            let outputs = result.outputs.to_value_map();
            let error = result.error.as_ref().map(|e| e.message.clone());

            let _ = self
                .execute_hooks(HookPoint::AfterNodeExecute, || {
                    serde_json::json!({
                      "event": "after_node_execute",
                      "node_id": node_id,
                      "node_type": node_type,
                      "node_title": node_title,
                      "status": status,
                      "outputs": outputs,
                      "error": error,
                    })
                })
                .await?;
            Ok(())
        }

        async fn apply_before_variable_write_hooks(
            &self,
            node_id: &str,
            selector: &Selector,
            value: &mut Segment,
        ) -> WorkflowResult<()> {
            let selector = selector.clone();
            let node_id = node_id.to_string();
            let original = value.to_value();

            let results = self
                .execute_hooks(HookPoint::BeforeVariableWrite, || {
                    serde_json::json!({
                      "event": "before_variable_write",
                      "node_id": node_id,
                      "selector": selector,
                      "value": original,
                    })
                })
                .await?;

            if let Some(last) = results.into_iter().rev().find(|v| !v.is_null()) {
                *value = Segment::from_value(&last);
            }
            Ok(())
        }

        fn mark_pool_dirty(&self) {
            self.cache.mark_dirty();
        }
    }

    pub fn new_gate(
        variable_pool: Arc<RwLock<VariablePool>>,
        event_emitter: EventEmitter,
        plugin_registry: Option<Arc<PluginRegistry>>,
    ) -> Arc<dyn PluginGate> {
        Arc::new(RealPluginGate::new(
            variable_pool,
            event_emitter,
            plugin_registry,
        ))
    }
}

#[cfg(feature = "plugin-system")]
pub use real::new_gate as new_plugin_gate_with_registry;

#[cfg(not(feature = "plugin-system"))]
pub fn new_plugin_gate(
    _event_emitter: EventEmitter,
    _variable_pool: Arc<parking_lot::RwLock<VariablePool>>,
) -> Arc<dyn PluginGate> {
    Arc::new(NoopPluginGate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_noop_plugin_gate_before_workflow() {
        let gate = NoopPluginGate;
        assert!(gate.emit_before_workflow_hooks().await.is_ok());
    }

    #[tokio::test]
    async fn test_noop_plugin_gate_after_workflow() {
        let gate = NoopPluginGate;
        let outputs = HashMap::new();
        assert!(gate.emit_after_workflow_hooks(&outputs, 0).await.is_ok());
    }

    #[tokio::test]
    async fn test_noop_plugin_gate_before_node() {
        let gate = NoopPluginGate;
        let config = serde_json::json!({});
        assert!(gate
            .emit_before_node_hooks("n1", "code", "Code", &config)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_noop_plugin_gate_after_node() {
        let gate = NoopPluginGate;
        let result = NodeRunResult::default();
        assert!(gate
            .emit_after_node_hooks("n1", "code", "Code", &result)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_noop_plugin_gate_variable_write() {
        let gate = NoopPluginGate;
        let sel = Selector::new("n1", "out");
        let mut val = Segment::String("test".to_string());
        assert!(gate
            .apply_before_variable_write_hooks("n1", &sel, &mut val)
            .await
            .is_ok());
        assert_eq!(val, Segment::String("test".to_string()));
    }

    #[test]
    fn test_noop_plugin_gate_mark_pool_dirty() {
        let gate = NoopPluginGate;
        gate.mark_pool_dirty(); // no panic = pass
    }
}
