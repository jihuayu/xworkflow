use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use std::sync::Arc;

use crate::compiler::compiled_workflow::CompiledWorkflow;
use crate::compiler::WorkflowCompiler;
use crate::dsl::DslFormat;
use crate::error::WorkflowError;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct CacheKey {
    pub group_id: String,
    pub content_hash: u64,
}

#[derive(Debug, Clone)]
pub struct WorkflowCacheConfig {
    pub max_entries_per_group: usize,
    pub max_total_entries: usize,
    pub ttl: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub group_count: usize,
    pub total_entries: usize,
}

#[derive(Debug, Clone)]
pub struct GroupCacheStats {
    pub entries: usize,
}

struct CacheEntry {
    compiled: Arc<CompiledWorkflow>,
    created_at: Instant,
    last_accessed: Instant,
    access_count: u64,
}

struct GroupCache {
    entries: HashMap<u64, CacheEntry>,
    lru_order: VecDeque<u64>,
}

impl GroupCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            lru_order: VecDeque::new(),
        }
    }

    fn touch(&mut self, key: u64) {
        self.lru_order.retain(|k| *k != key);
        self.lru_order.push_back(key);
    }

    fn insert(&mut self, key: u64, entry: CacheEntry) {
        self.entries.insert(key, entry);
        self.touch(key);
    }

    fn remove(&mut self, key: u64) {
        self.entries.remove(&key);
        self.lru_order.retain(|k| *k != key);
    }

    fn pop_lru(&mut self) -> Option<u64> {
        let key = self.lru_order.pop_front()?;
        self.entries.remove(&key);
        Some(key)
    }
}

pub struct WorkflowCache {
    groups: DashMap<String, GroupCache>,
    config: WorkflowCacheConfig,
}

impl WorkflowCache {
    pub fn new(config: WorkflowCacheConfig) -> Self {
        Self {
            groups: DashMap::new(),
            config,
        }
    }

    pub fn get_or_compile(
        &self,
        group_id: &str,
        content: &str,
        format: DslFormat,
    ) -> Result<Arc<CompiledWorkflow>, WorkflowError> {
        let content_hash = Self::hash_content(content);
        if let Some(mut group_cache) = self.groups.get_mut(group_id) {
            let mut cached: Option<Arc<CompiledWorkflow>> = None;
            let mut expired = false;
            if let Some(entry) = group_cache.entries.get_mut(&content_hash) {
                if self.is_expired(entry) {
                    expired = true;
                } else {
                    entry.last_accessed = Instant::now();
                    entry.access_count += 1;
                    cached = Some(entry.compiled.clone());
                }
            }
            if expired {
                group_cache.remove(content_hash);
            }
            if let Some(compiled) = cached {
                group_cache.touch(content_hash);
                return Ok(compiled);
            }
        }

        let compiled = Arc::new(WorkflowCompiler::compile(content, format)?);
        self.insert_entry(group_id, content_hash, compiled.clone());
        Ok(compiled)
    }

    pub fn get_or_compile_by_id(
        &self,
        group_id: &str,
        workflow_id: &str,
        content: &str,
        format: DslFormat,
    ) -> Result<Arc<CompiledWorkflow>, WorkflowError> {
        let content_hash = Self::hash_workflow_id(workflow_id);
        if let Some(mut group_cache) = self.groups.get_mut(group_id) {
            let mut cached: Option<Arc<CompiledWorkflow>> = None;
            let mut expired = false;
            if let Some(entry) = group_cache.entries.get_mut(&content_hash) {
                if self.is_expired(entry) {
                    expired = true;
                } else {
                    entry.last_accessed = Instant::now();
                    entry.access_count += 1;
                    cached = Some(entry.compiled.clone());
                }
            }
            if expired {
                group_cache.remove(content_hash);
            }
            if let Some(compiled) = cached {
                group_cache.touch(content_hash);
                return Ok(compiled);
            }
        }

        let compiled = Arc::new(WorkflowCompiler::compile(content, format)?);
        self.insert_entry(group_id, content_hash, compiled.clone());
        Ok(compiled)
    }

    pub fn invalidate(&self, group_id: &str, content_hash: u64) {
        if let Some(mut group_cache) = self.groups.get_mut(group_id) {
            group_cache.remove(content_hash);
        }
    }

    pub fn clear_group(&self, group_id: &str) {
        self.groups.remove(group_id);
    }

    pub fn clear_all(&self) {
        self.groups.clear();
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            group_count: self.groups.len(),
            total_entries: self
                .groups
                .iter()
                .map(|g| g.entries.len())
                .sum(),
        }
    }

    pub fn group_stats(&self, group_id: &str) -> Option<GroupCacheStats> {
        self.groups.get(group_id).map(|g| GroupCacheStats {
            entries: g.entries.len(),
        })
    }

    fn insert_entry(&self, group_id: &str, content_hash: u64, compiled: Arc<CompiledWorkflow>) {
        let entry = CacheEntry {
            compiled,
            created_at: Instant::now(),
            last_accessed: Instant::now(),
            access_count: 1,
        };

        {
            let mut group_cache = self
                .groups
                .entry(group_id.to_string())
                .or_insert_with(GroupCache::new);
            group_cache.insert(content_hash, entry);
            self.maybe_evict_group(&mut group_cache);
        }
        self.maybe_evict_global();
    }

    fn maybe_evict_group(&self, group_cache: &mut GroupCache) {
        if self.config.max_entries_per_group == 0 {
            return;
        }
        while group_cache.entries.len() > self.config.max_entries_per_group {
            group_cache.pop_lru();
        }
    }

    fn maybe_evict_global(&self) {
        if self.config.max_total_entries == 0 {
            return;
        }
        loop {
            let total_entries: usize = self
                .groups
                .iter()
                .map(|g| g.entries.len())
                .sum();
            if total_entries <= self.config.max_total_entries {
                break;
            }

            let mut oldest: Option<(String, u64, Instant)> = None;
            for entry in self.groups.iter() {
                for (hash, cache_entry) in entry.entries.iter() {
                    let ts = cache_entry.last_accessed;
                    if oldest
                        .as_ref()
                        .map(|(_, _, oldest_ts)| ts < *oldest_ts)
                        .unwrap_or(true)
                    {
                        oldest = Some((entry.key().clone(), *hash, ts));
                    }
                }
            }

            let Some((group_id, hash, _)) = oldest else {
                break;
            };

            if let Some(mut group_cache) = self.groups.get_mut(&group_id) {
                group_cache.remove(hash);
            } else {
                break;
            }
        }
    }

    fn is_expired(&self, entry: &CacheEntry) -> bool {
        if let Some(ttl) = self.config.ttl {
            entry.created_at.elapsed() > ttl
        } else {
            false
        }
    }

    fn hash_content(content: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.as_bytes().hash(&mut hasher);
        hasher.finish()
    }

    fn hash_workflow_id(workflow_id: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        "workflow_id".hash(&mut hasher);
        workflow_id.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(all(test, feature = "workflow-cache", feature = "builtin-core-nodes"))]
mod tests {
        use super::*;
    use crate::dsl::DslFormat;
        use crate::scheduler::ExecutionStatus;
        use serde_json::Value;
        use std::collections::HashMap;
    use std::sync::Arc;

        fn sample_yaml() -> &'static str {
        r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          label: Q
          type: string
          required: true
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["start", "query"]
edges:
  - source: start
    target: end
"#
        }

        #[tokio::test]
        async fn test_cache_reuses_compiled_workflow() {
        let cache = WorkflowCache::new(WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: None,
        });

        let compiled_a = cache
            .get_or_compile("group_a", sample_yaml(), DslFormat::Yaml)
            .unwrap();
        let compiled_b = cache
            .get_or_compile("group_a", sample_yaml(), DslFormat::Yaml)
            .unwrap();

        assert!(Arc::ptr_eq(&compiled_a, &compiled_b));

        let mut inputs = HashMap::new();
        inputs.insert("query".to_string(), Value::String("cached".into()));
        let handle = compiled_a.runner().user_inputs(inputs).run().await.unwrap();
        let status = handle.wait().await;
        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(outputs.get("result"), Some(&Value::String("cached".into())));
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
        }
}
