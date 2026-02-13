use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::{Arc, OnceLock, RwLock};

use super::client::{McpClient, RmcpClient};
use super::config::{McpServerConfig, McpTransport};
use super::error::McpError;

pub struct McpConnectionPool {
    connections: HashMap<u64, Arc<dyn McpClient>>,
}

fn mock_connections() -> &'static RwLock<HashMap<u64, Arc<dyn McpClient>>> {
    static MOCK_CONNECTIONS: OnceLock<RwLock<HashMap<u64, Arc<dyn McpClient>>>> = OnceLock::new();
    MOCK_CONNECTIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn register_mock_connection(config: &McpServerConfig, client: Arc<dyn McpClient>) {
    let key = McpConnectionPool::config_hash(config);
    let mut guard = mock_connections()
        .write()
        .expect("mock MCP registry poisoned");
    guard.insert(key, client);
}

pub fn clear_mock_connections() {
    let mut guard = mock_connections()
        .write()
        .expect("mock MCP registry poisoned");
    guard.clear();
}

impl McpConnectionPool {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
        }
    }

    pub async fn get_or_connect(
        &mut self,
        config: &McpServerConfig,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        let key = Self::config_hash(config);
        if let Some(client) = self.connections.get(&key) {
            return Ok(client.clone());
        }

        if let Some(client) = mock_connections()
            .read()
            .expect("mock MCP registry poisoned")
            .get(&key)
            .cloned()
        {
            self.connections.insert(key, client.clone());
            return Ok(client);
        }

        let client: Arc<dyn McpClient> = match &config.transport {
            McpTransport::Stdio { command, args, env } => Arc::new(
                RmcpClient::connect_stdio(command, args, env).await?,
            ),
            McpTransport::Http { url, headers } => Arc::new(
                RmcpClient::connect_http(url, headers).await?,
            ),
        };

        self.connections.insert(key, client.clone());
        Ok(client)
    }

    pub fn insert_connection(&mut self, config: &McpServerConfig, client: Arc<dyn McpClient>) {
        let key = Self::config_hash(config);
        self.connections.insert(key, client);
    }

    pub async fn close_all(&mut self) {
        let clients: Vec<Arc<dyn McpClient>> = self.connections.drain().map(|(_, c)| c).collect();
        for client in clients {
            let _ = client.close().await;
        }
    }

    fn config_hash(config: &McpServerConfig) -> u64 {
        let mut hasher = DefaultHasher::new();
        if let Ok(bytes) = serde_json::to_vec(config) {
            hasher.write(&bytes);
        }
        hasher.finish()
    }
}

impl Default for McpConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}
