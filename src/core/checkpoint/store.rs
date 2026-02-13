use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::types::Checkpoint;

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("Serialization error: {0}")]
    SerializationError(String),
    #[error("Storage error: {0}")]
    StorageError(String),
    #[error("Checkpoint not found for workflow: {0}")]
    NotFound(String),
    #[error("Checkpoint corrupted: {0}")]
    Corrupted(String),
    #[error("Resume rejected for workflow '{workflow_id}':\n{diagnostic}")]
    ResumeRejected {
        workflow_id: String,
        diagnostic: String,
    },
}

#[async_trait]
pub trait CheckpointStore: Send + Sync {
    async fn save(&self, workflow_id: &str, checkpoint: &Checkpoint)
        -> Result<(), CheckpointError>;
    async fn load(&self, workflow_id: &str) -> Result<Option<Checkpoint>, CheckpointError>;
    async fn delete(&self, workflow_id: &str) -> Result<(), CheckpointError>;
}

#[derive(Default)]
pub struct MemoryCheckpointStore {
    data: tokio::sync::RwLock<HashMap<String, Checkpoint>>,
}

impl MemoryCheckpointStore {
    pub fn new() -> Self {
        Self {
            data: tokio::sync::RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl CheckpointStore for MemoryCheckpointStore {
    async fn save(
        &self,
        workflow_id: &str,
        checkpoint: &Checkpoint,
    ) -> Result<(), CheckpointError> {
        self.data
            .write()
            .await
            .insert(workflow_id.to_string(), checkpoint.clone());
        Ok(())
    }

    async fn load(&self, workflow_id: &str) -> Result<Option<Checkpoint>, CheckpointError> {
        Ok(self.data.read().await.get(workflow_id).cloned())
    }

    async fn delete(&self, workflow_id: &str) -> Result<(), CheckpointError> {
        self.data.write().await.remove(workflow_id);
        Ok(())
    }
}

pub struct FileCheckpointStore {
    dir: PathBuf,
}

impl FileCheckpointStore {
    pub fn new(dir: impl AsRef<Path>) -> Result<Self, CheckpointError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| CheckpointError::StorageError(e.to_string()))?;
        Ok(Self { dir })
    }

    fn path_for(&self, workflow_id: &str) -> PathBuf {
        self.dir.join(format!("{}.checkpoint.json", workflow_id))
    }
}

#[async_trait]
impl CheckpointStore for FileCheckpointStore {
    async fn save(
        &self,
        workflow_id: &str,
        checkpoint: &Checkpoint,
    ) -> Result<(), CheckpointError> {
        let path = self.path_for(workflow_id);
        let bytes = serde_json::to_vec(checkpoint)
            .map_err(|e| CheckpointError::SerializationError(e.to_string()))?;
        tokio::fs::write(path, bytes)
            .await
            .map_err(|e| CheckpointError::StorageError(e.to_string()))
    }

    async fn load(&self, workflow_id: &str) -> Result<Option<Checkpoint>, CheckpointError> {
        let path = self.path_for(workflow_id);
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(CheckpointError::StorageError(e.to_string())),
        };

        let checkpoint = serde_json::from_slice::<Checkpoint>(&bytes)
            .map_err(|e| CheckpointError::Corrupted(e.to_string()))?;
        Ok(Some(checkpoint))
    }

    async fn delete(&self, workflow_id: &str) -> Result<(), CheckpointError> {
        let path = self.path_for(workflow_id);
        let _ = tokio::fs::remove_file(path).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::checkpoint::types::{
        Checkpoint, ConsumedResources, ContextFingerprint, SerializableEdgeState,
    };

    fn sample_checkpoint() -> Checkpoint {
        Checkpoint {
            workflow_id: "wf-1".to_string(),
            execution_id: "exec-1".to_string(),
            created_at: 1,
            completed_node_id: "node-1".to_string(),
            node_states: HashMap::<String, SerializableEdgeState>::new(),
            edge_states: HashMap::<String, SerializableEdgeState>::new(),
            ready_queue: vec!["node-2".to_string()],
            ready_predecessor: HashMap::new(),
            variables: HashMap::new(),
            step_count: 3,
            exceptions_count: 0,
            final_outputs: HashMap::new(),
            elapsed_secs: 10,
            consumed_resources: Option::<ConsumedResources>::None,
            context_fingerprint: Option::<ContextFingerprint>::None,
        }
    }

    #[tokio::test]
    async fn test_memory_checkpoint_store_save_load_delete() {
        let store = MemoryCheckpointStore::new();
        let cp = sample_checkpoint();

        store.save("wf-1", &cp).await.unwrap();
        let loaded = store.load("wf-1").await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().completed_node_id, "node-1");

        store.delete("wf-1").await.unwrap();
        assert!(store.load("wf-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_file_checkpoint_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCheckpointStore::new(dir.path()).unwrap();
        let cp = sample_checkpoint();

        store.save("wf-1", &cp).await.unwrap();
        let loaded = store.load("wf-1").await.unwrap().unwrap();
        assert_eq!(loaded.execution_id, "exec-1");

        store.delete("wf-1").await.unwrap();
        assert!(store.load("wf-1").await.unwrap().is_none());
    }
}
