use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use wasmtime::{Engine, Linker, Module, Store};
use anyhow::anyhow;

use crate::llm::error::LlmError;
use crate::llm::types::{
    ChatCompletionRequest, ChatCompletionResponse, ModelInfo, ProviderInfo, StreamChunk,
};
use crate::llm::LlmProvider;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmProviderManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub models: Vec<ModelInfo>,
    pub wasm_file: String,
    #[serde(default)]
    pub config_schema: HashMap<String, WasmConfigField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmConfigField {
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub env_var: Option<String>,
}

struct WasmProviderState {
    config: HashMap<String, String>,
}

#[derive(Clone)]
pub struct WasmLlmProvider {
    manifest: WasmProviderManifest,
    engine: Engine,
    module: Module,
    config: HashMap<String, String>,
}

impl WasmLlmProvider {
    pub fn new(manifest: WasmProviderManifest, wasm_bytes: &[u8]) -> Result<Self, LlmError> {
        let mut cfg = wasmtime::Config::new();
        cfg.consume_fuel(false);
        let engine = Engine::new(&cfg)
            .map_err(|e| LlmError::WasmError(e.to_string()))?;
        let module = Module::new(&engine, wasm_bytes)
            .map_err(|e| LlmError::WasmError(e.to_string()))?;
        let config = resolve_config(&manifest)?;
        Ok(Self {
            manifest,
            engine,
            module,
            config,
        })
    }

    fn execute(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse, LlmError> {
        let mut linker = Linker::new(&self.engine);
        register_host_functions(&mut linker)?;

        let state = WasmProviderState {
            config: self.config.clone(),
        };
        let mut store = Store::new(&self.engine, state);

        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| LlmError::WasmError(e.to_string()))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| LlmError::WasmError("Missing export: memory".into()))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "alloc")
            .map_err(|_| LlmError::WasmError("Missing export: alloc".into()))?;
        let dealloc = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "dealloc")
            .map_err(|_| LlmError::WasmError("Missing export: dealloc".into()))?;
        let handler = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "chat_completion")
            .map_err(|_| LlmError::WasmError("Missing export: chat_completion".into()))?;

        let payload = serde_json::json!({
            "request": request,
            "config": self.config.clone(),
        });
        let req_bytes = serde_json::to_vec(&payload)
            .map_err(|e| LlmError::SerializationError(e.to_string()))?;
        let req_len = req_bytes.len() as i32;
        let req_ptr = alloc
            .call(&mut store, req_len)
            .map_err(|e| LlmError::WasmError(e.to_string()))?;
        memory
            .write(&mut store, req_ptr as usize, &req_bytes)
            .map_err(|e| LlmError::WasmError(e.to_string()))?;

        let result_ptr = handler
            .call(&mut store, (req_ptr, req_len))
            .map_err(|e| LlmError::WasmError(e.to_string()))?;

        let mut header = [0u8; 8];
        memory
            .read(&mut store, result_ptr as usize, &mut header)
            .map_err(|e| LlmError::WasmError(e.to_string()))?;
        let out_ptr = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let out_len = i32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;

        let mut out_bytes = vec![0u8; out_len];
        memory
            .read(&mut store, out_ptr, &mut out_bytes)
            .map_err(|e| LlmError::WasmError(e.to_string()))?;
        let output: ChatCompletionResponse = serde_json::from_slice(&out_bytes)
            .map_err(|e| LlmError::SerializationError(e.to_string()))?;

        let _ = dealloc.call(&mut store, (req_ptr, req_len));
        let _ = dealloc.call(&mut store, (out_ptr as i32, out_len as i32));
        let _ = dealloc.call(&mut store, (result_ptr, 8));

        Ok(output)
    }
}

#[async_trait]
impl LlmProvider for WasmLlmProvider {
    fn id(&self) -> &str {
        &self.manifest.id
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.manifest.id.clone(),
            name: self.manifest.name.clone(),
            models: self.manifest.models.clone(),
        }
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LlmError> {
        let provider = self.clone();
        tokio::task::spawn_blocking(move || provider.execute(&request))
            .await
            .map_err(|e| LlmError::WasmError(e.to_string()))?
    }

    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
        chunk_tx: mpsc::Sender<StreamChunk>,
    ) -> Result<ChatCompletionResponse, LlmError> {
        let resp = self.chat_completion(request).await?;
        let _ = chunk_tx
            .send(StreamChunk {
                delta: resp.content.clone(),
                finish_reason: resp.finish_reason.clone(),
                usage: Some(resp.usage.clone()),
            })
            .await;
        Ok(resp)
    }
}

fn resolve_config(manifest: &WasmProviderManifest) -> Result<HashMap<String, String>, LlmError> {
    let mut config = HashMap::new();
    for (key, field) in &manifest.config_schema {
        let mut value = field
            .env_var
            .as_ref()
            .and_then(|env| std::env::var(env).ok());
        if value.is_none() {
            value = field.default.clone();
        }
        if field.required && value.is_none() {
            return Err(LlmError::InvalidRequest(format!(
                "Missing required config: {}",
                key
            )));
        }
        if let Some(v) = value {
            config.insert(key.clone(), v);
        }
    }
    Ok(config)
}

fn register_host_functions(linker: &mut Linker<WasmProviderState>) -> Result<(), LlmError> {
    linker
        .func_wrap("xworkflow", "http_request", http_request_impl)
        .map_err(|e| LlmError::WasmError(e.to_string()))?;
    linker
        .func_wrap("xworkflow", "log", log_impl)
        .map_err(|e| LlmError::WasmError(e.to_string()))?;
    Ok(())
}

fn http_request_impl(
    mut caller: wasmtime::Caller<'_, WasmProviderState>,
    req_ptr: i32,
    req_len: i32,
) -> Result<(i32, i32), anyhow::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow!("Missing export: memory"))?;

    let mut req_bytes = vec![0u8; req_len as usize];
    memory
        .read(&mut caller, req_ptr as usize, &mut req_bytes)
        .map_err(|e| anyhow!(e.to_string()))?;
    let req_json: Value = serde_json::from_slice(&req_bytes)
        .map_err(|e| anyhow!(e.to_string()))?;

    let method = req_json
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET");
    let url = req_json.get("url").and_then(|v| v.as_str()).unwrap_or("");

    let client = reqwest::blocking::Client::new();
    let mut builder = match method.to_uppercase().as_str() {
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        "PATCH" => client.patch(url),
        "HEAD" => client.head(url),
        _ => client.get(url),
    };

    if let Some(headers) = req_json.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in headers {
            if let Some(val) = v.as_str() {
                builder = builder.header(k, val);
            }
        }
    }

    if let Some(body) = req_json.get("body").and_then(|v| v.as_str()) {
        builder = builder.body(body.to_string());
    }

    let resp = builder
        .send()
        .map_err(|e| anyhow!(e.to_string()))?;

    let status = resp.status().as_u16();
    let mut resp_headers = serde_json::Map::new();
    for (k, v) in resp.headers() {
        if let Ok(val) = v.to_str() {
            resp_headers.insert(k.to_string(), Value::String(val.to_string()));
        }
    }
    let body = resp.text().unwrap_or_default();

    let result = serde_json::json!({
        "status": status,
        "headers": resp_headers,
        "body": body,
    });

    let result_bytes = serde_json::to_vec(&result)
        .map_err(|e| anyhow!(e.to_string()))?;
    let result_len = result_bytes.len() as i32;

    let alloc = caller
        .get_export("alloc")
        .and_then(|e| e.into_func())
        .ok_or_else(|| anyhow!("Missing export: alloc"))?;
    let alloc = alloc
        .typed::<i32, i32>(&mut caller)
        .map_err(|e| anyhow!(e.to_string()))?;
    let result_ptr = alloc
        .call(&mut caller, result_len)
        .map_err(|e| anyhow!(e.to_string()))?;

    memory
        .write(&mut caller, result_ptr as usize, &result_bytes)
        .map_err(|e| anyhow!(e.to_string()))?;

    Ok((result_ptr, result_len))
}

fn log_impl(
    mut caller: wasmtime::Caller<'_, WasmProviderState>,
    level: i32,
    msg_ptr: i32,
    msg_len: i32,
) -> Result<(), anyhow::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow!("Missing export: memory"))?;
    let mut buf = vec![0u8; msg_len as usize];
    memory
        .read(&mut caller, msg_ptr as usize, &mut buf)
        .map_err(|e| anyhow!(e.to_string()))?;
    let msg = String::from_utf8_lossy(&buf);

    match level {
        0 => tracing::trace!("[wasm-llm] {}", msg),
        1 => tracing::debug!("[wasm-llm] {}", msg),
        2 => tracing::info!("[wasm-llm] {}", msg),
        3 => tracing::warn!("[wasm-llm] {}", msg),
        _ => tracing::error!("[wasm-llm] {}", msg),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_deserialize() {
        let json = r#"{
            "id": "test",
            "name": "Test",
            "version": "1.0.0",
            "models": [{"id": "m", "name": "M"}],
            "wasm_file": "provider.wasm",
            "config_schema": {"api_key": {"type": "string", "required": true}}
        }"#;
        let manifest: WasmProviderManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.id, "test");
    }
}
