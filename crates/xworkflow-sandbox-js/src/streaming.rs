use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use boa_engine::{Context, Source};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::builtins as js_builtins;
use crate::sandbox::BuiltinSandbox;
use xworkflow_types::sandbox::{
    CodeLanguage, SandboxError, SandboxRequest, SandboxResult, StreamingSandbox,
    StreamingSandboxHandle,
};

#[derive(Debug)]
struct CallbackInvoke {
    var_name: String,
    callback: String,
    arg: Option<Value>,
    resp: oneshot::Sender<Result<Option<Value>, String>>,
}

#[derive(Debug)]
enum RuntimeCommand {
    Invoke(CallbackInvoke),
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct JsStreamRuntime {
    tx: mpsc::Sender<RuntimeCommand>,
}

impl JsStreamRuntime {
    async fn invoke(
        &self,
        var_name: String,
        callback: &str,
        arg: Option<Value>,
    ) -> Result<Option<Value>, String> {
        let (resp_tx, resp_rx) = oneshot::channel();
        let cmd = RuntimeCommand::Invoke(CallbackInvoke {
            var_name,
            callback: callback.to_string(),
            arg,
            resp: resp_tx,
        });
        self.tx
            .send(cmd)
            .await
            .map_err(|_| "runtime closed".to_string())?;
        resp_rx.await.map_err(|_| "runtime dropped".to_string())?
    }

    pub async fn shutdown(&self) {
        let _ = self.tx.send(RuntimeCommand::Shutdown).await;
    }
}

impl Drop for JsStreamRuntime {
    fn drop(&mut self) {
        let _ = self.tx.try_send(RuntimeCommand::Shutdown);
    }
}

fn escape_js_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('\'', "\\'")
}

fn eval_js_to_string(context: &mut Context, code: &str) -> Result<String, String> {
    let result = context
        .eval(Source::from_bytes(code))
        .map_err(|e| format!("JS eval error: {}", e))?;
    let s = result
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .ok_or_else(|| "JS result is not string".to_string())?;
    Ok(s)
}

fn parse_json_result(result_str: &str) -> Result<Option<Value>, String> {
    if result_str == "__undefined__" {
        return Ok(None);
    }
    let val: Value =
        serde_json::from_str(result_str).map_err(|e| format!("Failed to parse JSON: {}", e))?;
    if val.is_null() {
        return Ok(None);
    }
    Ok(Some(val))
}

async fn spawn_js_stream_runtime_inner(
    code: String,
    inputs: Value,
    stream_vars: Vec<String>,
    exit_flag: Option<Arc<AtomicBool>>,
) -> Result<(Value, bool, JsStreamRuntime), SandboxError> {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<RuntimeCommand>(64);
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(Value, bool), String>>();

    let exit_flag_for_thread = exit_flag.clone();

    tokio::task::spawn_blocking(move || {
        let mut context = Context::default();
        if let Err(e) = js_builtins::register_all(&mut context) {
            let _ = ready_tx.send(Err(format!("Failed to register builtins: {}", e)));
            return;
        }

        if let Err(e) = context.eval(Source::from_bytes(&code)) {
            let _ = ready_tx.send(Err(format!("JS code eval error: {}", e)));
            return;
        }

        let inputs_json = serde_json::to_string(&inputs).unwrap_or("{}".into());
        let inputs_json_escaped = escape_js_string(&inputs_json);

        let mut stream_setup = String::new();
        stream_setup.push_str("var __inputs = JSON.parse('");
        stream_setup.push_str(&inputs_json_escaped);
        stream_setup.push_str("');\n");
        stream_setup
            .push_str("globalThis.__stream_callbacks__ = globalThis.__stream_callbacks__ || {};\n");

        for var_name in &stream_vars {
            let var_escaped = escape_js_string(var_name);
            stream_setup.push_str(&format!(
                "globalThis.__stream_callbacks__['{}'] = {{ on_chunk: null, on_end: null, on_error: null }};\n",
                var_escaped
            ));
            stream_setup.push_str(&format!("__inputs['{}'] = {{\n", var_escaped));
            stream_setup.push_str(&format!(
                "  on_chunk: function(fn) {{ globalThis.__stream_callbacks__['{}'].on_chunk = fn; return this; }},\n",
                var_escaped
            ));
            stream_setup.push_str(&format!(
                "  on_end: function(fn) {{ globalThis.__stream_callbacks__['{}'].on_end = fn; return this; }},\n",
                var_escaped
            ));
            stream_setup.push_str(&format!(
                "  on_error: function(fn) {{ globalThis.__stream_callbacks__['{}'].on_error = fn; return this; }}\n",
                var_escaped
            ));
            stream_setup.push_str("};\n");
        }
        stream_setup.push_str("globalThis.__stream_initial_output__ = main(__inputs);\n");
        stream_setup.push_str("globalThis.__stream_has_callbacks__ = false;\n");
        for var_name in &stream_vars {
            let var_escaped = escape_js_string(var_name);
            stream_setup.push_str(&format!(
                "if (typeof globalThis.__stream_callbacks__['{}'].on_chunk === 'function') {{ globalThis.__stream_has_callbacks__ = true; }}\n",
                var_escaped
            ));
        }

        if let Err(e) = context.eval(Source::from_bytes(&stream_setup)) {
            let _ = ready_tx.send(Err(format!("JS stream setup error: {}", e)));
            return;
        }

        let output_json = match eval_js_to_string(
            &mut context,
            "(function(){ var v = globalThis.__stream_initial_output__; if (v === undefined) return '__undefined__'; try { return JSON.stringify(v); } catch (e) { return '__undefined__'; } })()",
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        let initial_output = match parse_json_result(&output_json) {
            Ok(Some(v)) => v,
            Ok(None) => Value::Null,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };

        let has_callbacks = match eval_js_to_string(
            &mut context,
            "(function(){ return globalThis.__stream_has_callbacks__ ? 'true' : 'false'; })()",
        ) {
            Ok(v) => v == "true",
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };

        let _ = ready_tx.send(Ok((initial_output, has_callbacks)));

        while let Some(cmd) = cmd_rx.blocking_recv() {
            match cmd {
                RuntimeCommand::Invoke(inv) => {
                    let arg_json = inv
                        .arg
                        .map(|v| serde_json::to_string(&v).unwrap_or("null".into()))
                        .unwrap_or_else(|| "null".to_string());
                    let arg_escaped = escape_js_string(&arg_json);
                    let var_escaped = escape_js_string(&inv.var_name);
                    let cb_escaped = escape_js_string(&inv.callback);
                    let js = format!(
                        "(function() {{ var cb = globalThis.__stream_callbacks__ && globalThis.__stream_callbacks__['{}'] && globalThis.__stream_callbacks__['{}']['{}']; if (typeof cb !== 'function') return '__undefined__'; var arg = JSON.parse('{}'); var res = cb(arg); if (res === undefined) return '__undefined__'; try {{ return JSON.stringify(res); }} catch (e) {{ return '__undefined__'; }} }})()",
                        var_escaped, var_escaped, cb_escaped, arg_escaped
                    );
                    let result = match eval_js_to_string(&mut context, &js) {
                        Ok(s) => parse_json_result(&s),
                        Err(e) => Err(e),
                    };
                    let _ = inv.resp.send(result);
                }
                RuntimeCommand::Shutdown => break,
            }
        }

        if let Some(flag) = exit_flag_for_thread {
            flag.store(true, Ordering::SeqCst);
        }
    });

    let (initial_output, has_callbacks) = ready_rx
        .await
        .map_err(|_| SandboxError::ExecutionError("JS runtime setup failed".into()))?
        .map_err(SandboxError::ExecutionError)?;

    Ok((
        initial_output,
        has_callbacks,
        JsStreamRuntime { tx: cmd_tx },
    ))
}

#[doc(hidden)]
pub async fn spawn_js_stream_runtime_with_exit_flag(
    code: String,
    inputs: Value,
    stream_vars: Vec<String>,
) -> Result<(Value, bool, JsStreamRuntime, Arc<AtomicBool>), SandboxError> {
    let exit_flag = Arc::new(AtomicBool::new(false));
    let (initial_output, has_callbacks, runtime) =
        spawn_js_stream_runtime_inner(code, inputs, stream_vars, Some(exit_flag.clone())).await?;
    Ok((initial_output, has_callbacks, runtime, exit_flag))
}

struct JsStreamingHandle {
    runtime: JsStreamRuntime,
}

#[async_trait::async_trait]
impl StreamingSandboxHandle for JsStreamingHandle {
    async fn send_chunk(
        &self,
        var_name: &str,
        chunk: &Value,
    ) -> Result<Option<Value>, SandboxError> {
        self.runtime
            .invoke(var_name.to_string(), "on_chunk", Some(chunk.clone()))
            .await
            .map_err(SandboxError::ExecutionError)
    }

    async fn end_stream(
        &self,
        var_name: &str,
        value: &Value,
    ) -> Result<Option<Value>, SandboxError> {
        self.runtime
            .invoke(var_name.to_string(), "on_end", Some(value.clone()))
            .await
            .map_err(SandboxError::ExecutionError)
    }

    async fn error_stream(
        &self,
        var_name: &str,
        error: &str,
    ) -> Result<Option<Value>, SandboxError> {
        self.runtime
            .invoke(
                var_name.to_string(),
                "on_error",
                Some(Value::String(error.to_string())),
            )
            .await
            .map_err(SandboxError::ExecutionError)
    }

    async fn finalize(self: Box<Self>) -> Result<SandboxResult, SandboxError> {
        self.runtime.shutdown().await;
        Ok(SandboxResult {
            success: true,
            output: Value::Null,
            stdout: String::new(),
            stderr: String::new(),
            execution_time: Duration::from_secs(0),
            memory_used: 0,
            error: None,
        })
    }
}

#[async_trait::async_trait]
impl StreamingSandbox for BuiltinSandbox {
    async fn execute_streaming(
        &self,
        request: SandboxRequest,
        stream_vars: Vec<String>,
    ) -> Result<(Value, bool, Box<dyn StreamingSandboxHandle>), SandboxError> {
        if request.language != CodeLanguage::JavaScript {
            return Err(SandboxError::UnsupportedLanguage(request.language));
        }

        self.validate_code(&request.code)?;

        let (initial_output, has_callbacks, runtime) =
            spawn_js_stream_runtime_inner(request.code, request.inputs, stream_vars, None).await?;

        Ok((
            initial_output,
            has_callbacks,
            Box::new(JsStreamingHandle { runtime }),
        ))
    }
}
