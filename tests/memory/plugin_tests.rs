use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use xworkflow::plugin_system::{
    Plugin, PluginCategory, PluginContext, PluginError, PluginMetadata, PluginSource,
};
use xworkflow::{ExecutionStatus, WorkflowRunner};

use super::helpers::{simple_workflow_schema, wait_for_condition, DropCounter};

struct ShutdownCountingPlugin {
    metadata: PluginMetadata,
    shutdowns: Arc<AtomicUsize>,
    _drop_counter: DropCounter,
}

impl ShutdownCountingPlugin {
    fn new(shutdowns: Arc<AtomicUsize>, drop_counter: DropCounter) -> Self {
        Self {
            metadata: PluginMetadata {
                id: "test.shutdown.plugin".into(),
                name: "Shutdown Plugin".into(),
                version: "0.1.0".into(),
                category: PluginCategory::Normal,
                description: "shutdown counter".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
            shutdowns,
            _drop_counter: drop_counter,
        }
    }
}

#[async_trait]
impl Plugin for ShutdownCountingPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, _context: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[tokio::test]
async fn test_plugin_shutdown_called_on_workflow_end() {
    let schema = simple_workflow_schema();
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let (drop_counter, drops) = DropCounter::new();
    let plugin = ShutdownCountingPlugin::new(shutdowns.clone(), drop_counter);

    let handle = WorkflowRunner::builder(schema)
        .plugin(Box::new(plugin))
        .run()
        .await
        .expect("workflow run");

    let status = handle.wait().await;
    assert!(matches!(status, ExecutionStatus::Completed(_)));

    wait_for_condition(
        "plugin shutdown",
        Duration::from_secs(2),
        Duration::from_millis(50),
        || shutdowns.load(Ordering::SeqCst) > 0,
    )
    .await;

    wait_for_condition(
        "plugin drop",
        Duration::from_secs(2),
        Duration::from_millis(50),
        || drops.load(Ordering::SeqCst) > 0,
    )
    .await;
}
