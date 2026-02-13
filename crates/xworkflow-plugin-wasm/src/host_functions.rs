use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::Value;
use wasmtime::{Caller, Linker, Memory, StoreLimits, StoreLimitsBuilder};

use super::error::PluginError;
use super::manifest::PluginCapabilities;

/// Host module name
pub const HOST_MODULE: &str = "xworkflow";

pub trait VariableAccess: Send + Sync {
    fn get(&self, selector: &Value) -> Result<Value>;
    fn set(&self, selector: &Value, value: &Value) -> Result<()>;
}

pub trait EventEmitter: Send + Sync {
    fn emit(&self, event_type: &str, data: Value) -> Result<()>;
}

/// Runtime state for each plugin invocation
pub struct PluginState {
    pub wasi: wasmtime_wasi::preview1::WasiP1Ctx,
    pub variable_access: Option<Arc<dyn VariableAccess>>,
    pub event_emitter: Option<Arc<dyn EventEmitter>>,
    pub capabilities: PluginCapabilities,
    pub limits: StoreLimits,
    pub plugin_id: String,
    pub host_buffers: Vec<Vec<u8>>,
}

impl PluginState {
    pub fn new(plugin_id: String, capabilities: PluginCapabilities) -> Self {
        Self {
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
            variable_access: None,
            event_emitter: None,
            capabilities,
            limits: StoreLimitsBuilder::new().build(),
            plugin_id,
            host_buffers: Vec::new(),
        }
    }

    pub fn with_variable_access(mut self, access: Arc<dyn VariableAccess>) -> Self {
        self.variable_access = Some(access);
        self
    }

    pub fn with_event_emitter(mut self, emitter: Arc<dyn EventEmitter>) -> Self {
        self.event_emitter = Some(emitter);
        self
    }
}

/// Register host functions into linker
pub fn register_host_functions(linker: &mut Linker<PluginState>) -> Result<(), PluginError> {
    linker
        .func_wrap(HOST_MODULE, "var_get", var_get_impl)
        .map_err(|e| PluginError::ExecutionError(e.to_string()))?;
    linker
        .func_wrap(HOST_MODULE, "var_set", var_set_impl)
        .map_err(|e| PluginError::ExecutionError(e.to_string()))?;
    linker
        .func_wrap(HOST_MODULE, "log", log_impl)
        .map_err(|e| PluginError::ExecutionError(e.to_string()))?;
    linker
        .func_wrap(HOST_MODULE, "emit_event", emit_event_impl)
        .map_err(|e| PluginError::ExecutionError(e.to_string()))?;
    linker
        .func_wrap(HOST_MODULE, "http_request", http_request_impl)
        .map_err(|e| PluginError::ExecutionError(e.to_string()))?;
    Ok(())
}

fn get_memory(caller: &mut Caller<'_, PluginState>) -> Result<Memory> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow!("Missing export: memory"))
}

fn read_bytes(caller: &mut Caller<'_, PluginState>, ptr: i32, len: i32) -> Result<Vec<u8>> {
    let memory = get_memory(caller)?;
    let mut buf = vec![0u8; len as usize];
    memory
        .read(caller, ptr as usize, &mut buf)
        .map_err(|e| anyhow!(e.to_string()))?;
    Ok(buf)
}

fn write_bytes(caller: &mut Caller<'_, PluginState>, ptr: i32, bytes: &[u8]) -> Result<()> {
    let memory = get_memory(caller)?;
    memory
        .write(caller, ptr as usize, bytes)
        .map_err(|e| anyhow!(e.to_string()))
}

fn alloc_in_guest(caller: &mut Caller<'_, PluginState>, size: i32) -> Result<i32> {
    let alloc = caller
        .get_export("alloc")
        .and_then(|e| e.into_func())
        .ok_or_else(|| anyhow!("Missing export: alloc"))?;
    let alloc = alloc
        .typed::<i32, i32>(&mut *caller)
        .map_err(|e| anyhow!(e.to_string()))?;
    alloc.call(caller, size).map_err(|e| anyhow!(e.to_string()))
}

fn write_json_to_guest(caller: &mut Caller<'_, PluginState>, value: &Value) -> Result<(i32, i32)> {
    let bytes = serde_json::to_vec(value).map_err(|e| anyhow!(e.to_string()))?;
    let len = bytes.len() as i32;
    let ptr = alloc_in_guest(caller, len)?;
    write_bytes(caller, ptr, &bytes)?;
    Ok((ptr, len))
}

fn var_get_impl(
    mut caller: Caller<'_, PluginState>,
    selector_ptr: i32,
    selector_len: i32,
) -> Result<(i32, i32)> {
    if !caller.data().capabilities.read_variables {
        return Err(anyhow!("Capability denied: read_variables"));
    }
    let selector_bytes = read_bytes(&mut caller, selector_ptr, selector_len)?;
    let selector: Value =
        serde_json::from_slice(&selector_bytes).map_err(|e| anyhow!(e.to_string()))?;
    let access = caller
        .data()
        .variable_access
        .as_ref()
        .ok_or_else(|| anyhow!("Variable access not available"))?;
    let value = access.get(&selector)?;
    write_json_to_guest(&mut caller, &value)
}

fn var_set_impl(
    mut caller: Caller<'_, PluginState>,
    selector_ptr: i32,
    selector_len: i32,
    value_ptr: i32,
    value_len: i32,
) -> Result<i32> {
    if !caller.data().capabilities.write_variables {
        return Ok(-1);
    }
    let selector_bytes = read_bytes(&mut caller, selector_ptr, selector_len)?;
    let selector: Value =
        serde_json::from_slice(&selector_bytes).map_err(|e| anyhow!(e.to_string()))?;
    let value_bytes = read_bytes(&mut caller, value_ptr, value_len)?;
    let value: Value = serde_json::from_slice(&value_bytes).map_err(|e| anyhow!(e.to_string()))?;
    let access = caller
        .data()
        .variable_access
        .as_ref()
        .ok_or_else(|| anyhow!("Variable access not available"))?;
    access.set(&selector, &value)?;
    Ok(0)
}

fn log_impl(
    mut caller: Caller<'_, PluginState>,
    level: i32,
    msg_ptr: i32,
    msg_len: i32,
) -> Result<()> {
    let msg_bytes = read_bytes(&mut caller, msg_ptr, msg_len)?;
    let msg = String::from_utf8_lossy(&msg_bytes);
    let prefix = &caller.data().plugin_id;
    match level {
        0 => tracing::trace!("[plugin:{}] {}", prefix, msg),
        1 => tracing::debug!("[plugin:{}] {}", prefix, msg),
        2 => tracing::info!("[plugin:{}] {}", prefix, msg),
        3 => tracing::warn!("[plugin:{}] {}", prefix, msg),
        _ => tracing::error!("[plugin:{}] {}", prefix, msg),
    }
    Ok(())
}

fn emit_event_impl(
    mut caller: Caller<'_, PluginState>,
    event_ptr: i32,
    event_len: i32,
) -> Result<i32> {
    if !caller.data().capabilities.emit_events {
        return Ok(-1);
    }
    let event_bytes = read_bytes(&mut caller, event_ptr, event_len)?;
    let payload: Value =
        serde_json::from_slice(&event_bytes).map_err(|e| anyhow!(e.to_string()))?;
    let event_type = payload
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    if let Some(emitter) = &caller.data().event_emitter {
        emitter.emit(&event_type, data)?;
        return Ok(0);
    }
    Ok(-1)
}

fn http_request_impl(
    mut caller: Caller<'_, PluginState>,
    req_ptr: i32,
    req_len: i32,
) -> Result<(i32, i32)> {
    if !caller.data().capabilities.http_access {
        return Err(anyhow!("Capability denied: http_access"));
    }
    let req_bytes = read_bytes(&mut caller, req_ptr, req_len)?;
    let req_json: Value = serde_json::from_slice(&req_bytes).map_err(|e| anyhow!(e.to_string()))?;

    let method = req_json
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET");
    let url = req_json.get("url").and_then(|v| v.as_str()).unwrap_or("");

    let client = reqwest::blocking::Client::new();
    let builder = match method.to_uppercase().as_str() {
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        "PATCH" => client.patch(url),
        "HEAD" => client.head(url),
        _ => client.get(url),
    };

    let mut builder = builder;
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

    let resp = builder.send().map_err(|e| anyhow!(e.to_string()))?;

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

    write_json_to_guest(&mut caller, &result)
}
