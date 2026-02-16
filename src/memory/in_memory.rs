use std::collections::HashMap;

use async_trait::async_trait;
use parking_lot::RwLock;
use uuid::Uuid;

use super::provider::{MemoryEntry, MemoryError, MemoryProvider, MemoryQuery, MemoryStoreParams};

pub struct InMemoryProvider {
    store: RwLock<HashMap<String, HashMap<String, MemoryEntry>>>,
}

impl InMemoryProvider {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryProvider for InMemoryProvider {
    async fn store(&self, params: MemoryStoreParams) -> Result<(), MemoryError> {
        let mut guard = self.store.write();
        let ns = guard.entry(params.namespace.clone()).or_default();
        let entry = MemoryEntry {
            id: Uuid::new_v4().to_string(),
            namespace: params.namespace,
            key: params.key.clone(),
            value: params.value,
            metadata: params.metadata,
            score: None,
        };
        ns.insert(params.key, entry);
        Ok(())
    }

    async fn recall(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>, MemoryError> {
        let guard = self.store.read();
        let Some(ns) = guard.get(&query.namespace) else {
            return Ok(Vec::new());
        };

        let mut entries: Vec<MemoryEntry> = if let Some(key) = query.key {
            ns.get(&key).cloned().into_iter().collect()
        } else {
            ns.values().cloned().collect()
        };

        if let Some(text) = query.query_text.as_ref() {
            if !text.is_empty() {
                let needle = text.to_lowercase();
                entries.retain(|entry| {
                    entry.key.to_lowercase().contains(&needle)
                        || entry.value.to_string().to_lowercase().contains(&needle)
                });
            }
        }

        if let Some(limit) = query.top_k {
            entries.truncate(limit);
        }

        Ok(entries)
    }

    async fn delete(&self, namespace: &str, key: &str) -> Result<(), MemoryError> {
        let mut guard = self.store.write();
        let Some(ns) = guard.get_mut(namespace) else {
            return Err(MemoryError::NotFound {
                namespace: namespace.to_string(),
                key: key.to_string(),
            });
        };
        if ns.remove(key).is_none() {
            return Err(MemoryError::NotFound {
                namespace: namespace.to_string(),
                key: key.to_string(),
            });
        }
        Ok(())
    }

    async fn clear_namespace(&self, namespace: &str) -> Result<(), MemoryError> {
        let mut guard = self.store.write();
        guard.remove(namespace);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn test_crud_and_namespace_isolation() {
        let provider = InMemoryProvider::new();
        provider
            .store(MemoryStoreParams {
                namespace: "conv:1".to_string(),
                key: "k1".to_string(),
                value: json!({"a": 1}),
                metadata: None,
            })
            .await
            .unwrap();
        provider
            .store(MemoryStoreParams {
                namespace: "conv:2".to_string(),
                key: "k1".to_string(),
                value: json!({"a": 2}),
                metadata: None,
            })
            .await
            .unwrap();

        let r1 = provider
            .recall(MemoryQuery {
                namespace: "conv:1".to_string(),
                key: Some("k1".to_string()),
                query_text: None,
                filter: None,
                top_k: None,
            })
            .await
            .unwrap();
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].value, json!({"a": 1}));

        provider.delete("conv:1", "k1").await.unwrap();
        let r1 = provider
            .recall(MemoryQuery {
                namespace: "conv:1".to_string(),
                key: None,
                query_text: None,
                filter: None,
                top_k: None,
            })
            .await
            .unwrap();
        assert!(r1.is_empty());
    }
}
