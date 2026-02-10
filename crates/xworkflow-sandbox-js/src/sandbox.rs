//! Built-in JavaScript sandbox implementation using boa_engine.
//!
//! Provides a safe JavaScript execution environment with:
//! - Resource limits (timeout, memory, code size)
//! - Dangerous code pattern detection
//! - Isolated execution contexts
//! - Standard JSON I/O via `main(inputs)` function convention

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use boa_engine::{Context, Source};
use serde_json::Value;

#[cfg(feature = "security")]
use crate::security::{AstCodeAnalyzer, CodeAnalyzer};

use xworkflow_types::sandbox::*;

/// Built-in sandbox configuration
#[derive(Clone, Debug)]
pub struct BuiltinSandboxConfig {
    /// Max code length (bytes)
    pub max_code_length: usize,

    /// Execution timeout
    pub default_timeout: Duration,

    /// Whether to enable console.log (captured in stdout)
    pub enable_console: bool,

    /// Estimated memory cap (bytes)
    pub max_memory_estimate: usize,

    /// Max output JSON bytes
    pub max_output_bytes: usize,

    /// Freeze global objects
    pub freeze_globals: bool,

    /// Allowed globals whitelist
    pub allowed_globals: Vec<String>,
}

impl Default for BuiltinSandboxConfig {
    fn default() -> Self {
        Self {
            max_code_length: 1_000_000, // 1MB
            default_timeout: Duration::from_secs(30),
            enable_console: true,
            max_memory_estimate: 32 * 1024 * 1024,
            max_output_bytes: 1 * 1024 * 1024,
            freeze_globals: true,
            allowed_globals: vec![
                "JSON".into(),
                "Math".into(),
                "parseInt".into(),
                "parseFloat".into(),
                "isNaN".into(),
                "isFinite".into(),
                "Number".into(),
                "String".into(),
                "Boolean".into(),
                "Array".into(),
                "Object".into(),
                "Error".into(),
                "encodeURIComponent".into(),
                "decodeURIComponent".into(),
                "encodeURI".into(),
                "decodeURI".into(),
                "btoa".into(),
                "atob".into(),
                "RegExp".into(),
                "Date".into(),
                "datetime".into(),
                "crypto".into(),
                "uuidv4".into(),
                "randomInt".into(),
                "randomFloat".into(),
                "randomBytes".into(),
            ],
        }
    }
}

/// Built-in JavaScript sandbox using boa_engine
pub struct BuiltinSandbox {
    config: BuiltinSandboxConfig,
    stats: Arc<RwLock<SandboxStats>>,
}

impl BuiltinSandbox {
    pub fn new(config: BuiltinSandboxConfig) -> Self {
        Self {
            config,
            stats: Arc::new(RwLock::new(SandboxStats::default())),
        }
    }

    /// Validate code for dangerous patterns
    fn validate_code(&self, code: &str) -> Result<(), SandboxError> {
        // 1. Check code length
        if code.len() > self.config.max_code_length {
            return Err(SandboxError::CodeTooLarge {
                max: self.config.max_code_length,
                actual: code.len(),
            });
        }

        // 2. AST-based dangerous code analysis
        #[cfg(feature = "security")]
        {
            let analyzer = AstCodeAnalyzer::default();
            let result = analyzer.analyze(code)?;
            if !result.is_safe {
                let summary = result
                    .violations
                    .iter()
                    .map(|v| v.description.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(SandboxError::DangerousCode(summary));
            }
        }

        Ok(())
    }

    /// Execute JavaScript code in boa_engine context
    fn execute_js(
        &self,
        code: &str,
        inputs: &Value,
        timeout: Duration,
    ) -> Result<SandboxResult, SandboxError> {
        let start_time = Instant::now();

        // Create a boa context
        let mut context = Context::default();

        crate::builtins::register_all(&mut context)
            .map_err(|e| SandboxError::InternalError(format!("Failed to register builtins: {}", e)))?;

        // Set up the runtime limit for instruction count (approximate timeout)
        // boa doesn't have a native timeout mechanism, so we measure wall time
        let _ = timeout; // used for checking below

        // Prepare the wrapper code:
        // 1. Define the user's code (which should define `function main(inputs)`)
        // 2. Call `main(inputs)` with the serialized inputs
        // 3. Wrap result as JSON string
        let inputs_json = serde_json::to_string(inputs)
            .map_err(|e| SandboxError::SerializationError(e.to_string()))?;

        if inputs_json.len() > self.config.max_memory_estimate {
            return Err(SandboxError::MemoryLimitExceeded);
        }

        // Console log capture - collect logs
        let console_setup = if self.config.enable_console {
            r#"
var __console_logs = [];
var console = {
    log: function() {
        var args = [];
        for (var i = 0; i < arguments.length; i++) {
            if (typeof arguments[i] === 'object') {
                args.push(JSON.stringify(arguments[i]));
            } else {
                args.push(String(arguments[i]));
            }
        }
        __console_logs.push(args.join(' '));
    },
    warn: function() { console.log.apply(null, arguments); },
    error: function() { console.log.apply(null, arguments); },
    info: function() { console.log.apply(null, arguments); }
};
"#
        } else {
            ""
        };

        let mut allowed = self.config.allowed_globals.clone();
        if self.config.enable_console {
            allowed.push("console".into());
            allowed.push("__console_logs".into());
        }
        allowed.sort();
        allowed.dedup();
        let allowed_list = allowed
            .iter()
            .map(|s| format!("\"{}\"", s))
            .collect::<Vec<_>>()
            .join(",");
        let security_setup = if self.config.freeze_globals || !allowed.is_empty() {
            format!(
                r#"
var __allowed_globals = new Set([{allowed_list}]);
var __global = (typeof globalThis !== 'undefined') ? globalThis : this;
var __Function = __global.Function;
Object.getOwnPropertyNames(__global).forEach(function(key) {{
    if (!__allowed_globals.has(key)) {{
        try {{ delete __global[key]; }} catch (e) {{ __global[key] = undefined; }}
    }}
}});
{freeze_globals}
"#,
                allowed_list = allowed_list,
                freeze_globals = if self.config.freeze_globals {
                    "Object.freeze(__global); Object.freeze(Object.prototype); Object.freeze(Array.prototype); if (typeof __Function !== 'undefined') { Object.freeze(__Function.prototype); }"
                } else {
                    ""
                }
            )
        } else {
            String::new()
        };

        let full_code = format!(
            r#"
{console_setup}
{security_setup}

// User code
{code}

// Execute main
(function() {{
    var __inputs = JSON.parse('{inputs_json_escaped}');
    var __result = main(__inputs);
    return JSON.stringify({{
        "__output": __result,
        "__console_logs": typeof __console_logs !== 'undefined' ? __console_logs : []
    }});
}})();
"#,
            console_setup = console_setup,
            security_setup = security_setup,
            code = code,
            inputs_json_escaped = inputs_json.replace('\\', "\\\\").replace('\'', "\\'"),
        );

        // Execute
        let result = context
            .eval(Source::from_bytes(&full_code))
            .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

        let execution_time = start_time.elapsed();

        // Check timeout
        if execution_time > timeout {
            return Err(SandboxError::ExecutionTimeout);
        }

        // Extract result
        let result_str = result
            .as_string()
            .map(|s| s.to_std_string_escaped())
            .ok_or_else(|| {
                SandboxError::ExecutionError(
                    "main() must return an object (got non-string JSON result)".to_string(),
                )
            })?;

        // Parse wrapper result
        let wrapper: Value = serde_json::from_str(&result_str)
            .map_err(|e| SandboxError::SerializationError(format!("Failed to parse result: {}", e)))?;

        let output = wrapper
            .get("__output")
            .cloned()
            .unwrap_or(Value::Null);

        let output_bytes = serde_json::to_vec(&output)
            .map_err(|e| SandboxError::SerializationError(e.to_string()))?;
        if output_bytes.len() > self.config.max_output_bytes {
            return Err(SandboxError::OutputTooLarge {
                max: self.config.max_output_bytes,
                actual: output_bytes.len(),
            });
        }

        let console_logs: Vec<String> = wrapper
            .get("__console_logs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let stdout = console_logs.join("\n");

        Ok(SandboxResult {
            success: true,
            output,
            stdout,
            stderr: String::new(),
            execution_time,
            memory_used: inputs_json.len() + result_str.len(),
            error: None,
        })
    }

    async fn update_stats(&self, result: &Result<SandboxResult, SandboxError>, execution_time: Duration) {
        let mut stats = self.stats.write().await;
        stats.total_executions += 1;
        match result {
            Ok(_) => stats.successful_executions += 1,
            Err(_) => stats.failed_executions += 1,
        }
        // Update avg execution time (simple running average)
        if stats.total_executions == 1 {
            stats.avg_execution_time = execution_time;
        } else {
            let total_ns = stats.avg_execution_time.as_nanos() as u64
                * (stats.total_executions - 1)
                + execution_time.as_nanos() as u64;
            stats.avg_execution_time =
                Duration::from_nanos(total_ns / stats.total_executions);
        }
    }
}

#[async_trait::async_trait]
impl CodeSandbox for BuiltinSandbox {
    fn sandbox_type(&self) -> SandboxType {
        SandboxType::Builtin
    }

    fn supported_languages(&self) -> Vec<CodeLanguage> {
        vec![CodeLanguage::JavaScript]
    }

    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError> {
        // Validate language
        if request.language != CodeLanguage::JavaScript {
            return Err(SandboxError::UnsupportedLanguage(request.language));
        }

        // Validate code
        self.validate_code(&request.code)?;

        let timeout = request.config.timeout;
        let start_time = Instant::now();

        // Execute in a blocking task since boa_engine is synchronous
        let code = request.code.clone();
        let inputs = request.inputs.clone();
        let config = self.config.clone();

        let result = tokio::task::spawn_blocking(move || {
            let sandbox = BuiltinSandbox {
                config,
                stats: Arc::new(RwLock::new(SandboxStats::default())),
            };
            sandbox.execute_js(&code, &inputs, timeout)
        })
        .await
        .map_err(|e| SandboxError::InternalError(format!("Task join error: {}", e)))?;

        let execution_time = start_time.elapsed();

        // Update stats
        self.update_stats(&result, execution_time).await;

        // Set the correct execution_time on the result
        result.map(|mut r| {
            r.execution_time = execution_time;
            r
        })
    }

    async fn validate(&self, code: &str, language: CodeLanguage) -> Result<(), SandboxError> {
        if language != CodeLanguage::JavaScript {
            return Err(SandboxError::UnsupportedLanguage(language));
        }
        self.validate_code(code)
    }

    async fn health_check(&self) -> Result<HealthStatus, SandboxError> {
        let request = SandboxRequest {
            code: "function main(inputs) { return { ok: true }; }".to_string(),
            language: CodeLanguage::JavaScript,
            inputs: serde_json::json!({}),
            config: ExecutionConfig {
                timeout: Duration::from_secs(5),
                ..ExecutionConfig::default()
            },
        };

        match self.execute(request).await {
            Ok(r) if r.success => Ok(HealthStatus::Healthy),
            Ok(_) => Ok(HealthStatus::Degraded),
            Err(_) => Ok(HealthStatus::Unhealthy),
        }
    }

    async fn get_stats(&self) -> Result<SandboxStats, SandboxError> {
        let stats = self.stats.read().await;
        Ok(stats.clone())
    }
}

// ================================
// Tests
// ================================

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use regex::Regex;
    use serde_json::json;
    use sha2::Sha256;
    use uuid::Uuid;

    fn default_sandbox() -> BuiltinSandbox {
        BuiltinSandbox::new(BuiltinSandboxConfig::default())
    }

    // ---- Basic execution tests ----

    #[tokio::test]
    async fn test_simple_return() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { ok: true }; }".to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["ok"], json!(true));
    }

    #[tokio::test]
    async fn test_arithmetic() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { result: inputs.a + inputs.b }; }"
                    .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "a": 10, "b": 32 }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["result"], json!(42));
    }

    #[tokio::test]
    async fn test_string_manipulation() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    return { greeting: "Hello, " + inputs.name + "!" };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "name": "World" }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["greeting"], json!("Hello, World!"));
    }

    #[tokio::test]
    async fn test_array_operations() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    var numbers = inputs.numbers;
                    var sum = 0;
                    for (var i = 0; i < numbers.length; i++) {
                        sum += numbers[i];
                    }
                    return { sum: sum, count: numbers.length };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "numbers": [1, 2, 3, 4, 5] }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["sum"], json!(15));
        assert_eq!(result.output["count"], json!(5));
    }

    #[tokio::test]
    async fn test_object_manipulation() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    var keys = Object.keys(inputs.data);
                    return { keys: keys, count: keys.length };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "data": { "a": 1, "b": 2, "c": 3 } }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["count"], json!(3));
    }

    #[tokio::test]
    async fn test_json_parse_stringify() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    var obj = JSON.parse(inputs.json_str);
                    obj.added = true;
                    return { result: obj };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "json_str": "{\"key\":\"value\"}" }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["result"]["key"], json!("value"));
        assert_eq!(result.output["result"]["added"], json!(true));
    }

    #[tokio::test]
    async fn test_math_functions() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    return {
                        abs: Math.abs(inputs.value),
                        floor: Math.floor(3.7),
                        ceil: Math.ceil(3.2),
                        max: Math.max(1, 2, 3),
                        min: Math.min(1, 2, 3)
                    };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "value": -5 }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["abs"], json!(5));
        assert_eq!(result.output["floor"], json!(3));
        assert_eq!(result.output["ceil"], json!(4));
        assert_eq!(result.output["max"], json!(3));
        assert_eq!(result.output["min"], json!(1));
    }

    #[tokio::test]
    async fn test_console_log() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    console.log("hello from sandbox");
                    console.log("value:", inputs.x);
                    return { done: true };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "x": 42 }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.stdout.contains("hello from sandbox"));
        assert!(result.stdout.contains("42"));
    }

    #[tokio::test]
    async fn test_complex_data_transform() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    var users = inputs.users;
                    var result = [];
                    for (var i = 0; i < users.length; i++) {
                        if (users[i].age >= 18) {
                            result.push({
                                name: users[i].name,
                                adult: true
                            });
                        }
                    }
                    return { adults: result, total: result.length };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({
                    "users": [
                        { "name": "Alice", "age": 25 },
                        { "name": "Bob", "age": 17 },
                        { "name": "Charlie", "age": 30 }
                    ]
                }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["total"], json!(2));
        let adults = result.output["adults"].as_array().unwrap();
        assert_eq!(adults.len(), 2);
    }

    // ---- Security tests ----

    #[tokio::test]
    #[cfg(feature = "security")]
    async fn test_reject_eval() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) { eval("1+1"); return {}; }"#.to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::DangerousCode(_) => {}
            other => panic!("Expected DangerousCode, got: {:?}", other),
        }
    }

    #[tokio::test]
    #[cfg(feature = "security")]
    async fn test_reject_require() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) { require("fs"); return {}; }"#.to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(feature = "security")]
    async fn test_reject_import() {
        let sandbox = default_sandbox();
        let result = sandbox
            .validate("import fs from 'fs';", CodeLanguage::JavaScript)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(feature = "security")]
    async fn test_reject_proto() {
        let sandbox = default_sandbox();
        let result = sandbox
            .validate(
                "function main(inputs) { inputs.__proto__.polluted = true; return {}; }",
                CodeLanguage::JavaScript,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(feature = "security")]
    async fn test_reject_function_constructor() {
        let sandbox = default_sandbox();
        let result = sandbox
            .validate(
                r#"function main(inputs) { var f = Function (\"return 1\"); return {}; }"#,
                CodeLanguage::JavaScript,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_code_too_large() {
        let sandbox = BuiltinSandbox::new(BuiltinSandboxConfig {
            max_code_length: 100,
            ..BuiltinSandboxConfig::default()
        });

        let code = "a".repeat(200);
        let result = sandbox
            .execute(SandboxRequest {
                code,
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::CodeTooLarge { max, actual } => {
                assert_eq!(max, 100);
                assert_eq!(actual, 200);
            }
            other => panic!("Expected CodeTooLarge, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_unsupported_language() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "print('hello')".to_string(),
                language: CodeLanguage::Python,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::UnsupportedLanguage(lang) => {
                assert_eq!(lang, CodeLanguage::Python);
            }
            other => panic!("Expected UnsupportedLanguage, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_execution_error_no_main() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "var x = 1;".to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await;

        // Should fail because main() is not defined
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_syntax_error() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs { return {}; }".to_string(), // missing )
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await;

        assert!(result.is_err());
    }

    // ---- Health / Stats tests ----

    #[tokio::test]
    async fn test_health_check() {
        let sandbox = default_sandbox();
        let health = sandbox.health_check().await.unwrap();
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[tokio::test]
    async fn test_stats_tracking() {
        let sandbox = default_sandbox();

        // Execute successfully
        sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { ok: true }; }".to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        // Execute with error
        let _ = sandbox
            .execute(SandboxRequest {
                code: "var x = 1;".to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await;

        let stats = sandbox.get_stats().await.unwrap();
        assert_eq!(stats.total_executions, 2);
        assert_eq!(stats.successful_executions, 1);
        assert_eq!(stats.failed_executions, 1);
    }

    // ---- Integration-style tests ----

    #[tokio::test]
    async fn test_text_analysis() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    var text = inputs.text;
                    var words = text.split(' ');
                    return {
                        word_count: words.length,
                        char_count: text.length,
                        has_hello: text.indexOf('hello') >= 0
                    };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "text": "hello world from sandbox" }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["word_count"], json!(4));
        assert_eq!(result.output["has_hello"], json!(true));
    }

    #[tokio::test]
    async fn test_data_validation() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    var data = inputs.data;
                    var errors = [];

                    if (!data.name || data.name.length === 0) {
                        errors.push("name is required");
                    }
                    if (typeof data.age !== 'number' || data.age < 0) {
                        errors.push("age must be a positive number");
                    }

                    return {
                        valid: errors.length === 0,
                        errors: errors
                    };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({
                    "data": { "name": "Alice", "age": 25 }
                }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["valid"], json!(true));
        assert_eq!(result.output["errors"], json!([]));
    }

    #[tokio::test]
    async fn test_number_processing() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    var numbers = inputs.numbers;
                    var sum = 0;
                    var max = numbers[0];
                    var min = numbers[0];
                    for (var i = 0; i < numbers.length; i++) {
                        sum += numbers[i];
                        if (numbers[i] > max) max = numbers[i];
                        if (numbers[i] < min) min = numbers[i];
                    }
                    return {
                        sum: sum,
                        avg: sum / numbers.length,
                        max: max,
                        min: min
                    };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "numbers": [10, 20, 30, 40, 50] }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["sum"], json!(150));
        assert_eq!(result.output["avg"], json!(30));
        assert_eq!(result.output["max"], json!(50));
        assert_eq!(result.output["min"], json!(10));
    }

    #[tokio::test]
    async fn test_conditional_logic() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    var score = inputs.score;
                    var grade;
                    if (score >= 90) grade = "A";
                    else if (score >= 80) grade = "B";
                    else if (score >= 70) grade = "C";
                    else if (score >= 60) grade = "D";
                    else grade = "F";
                    return { grade: grade };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "score": 85 }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["grade"], json!("B"));
    }

    #[tokio::test]
    async fn test_inputs_with_special_chars() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: r#"function main(inputs) {
                    return { text: inputs.text };
                }"#
                .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({ "text": "Hello 'world' \"test\" \\ new" }),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["text"], json!("Hello 'world' \"test\" \\ new"));
    }

    // ---- Builtin API tests ----

    #[tokio::test]
    async fn test_builtin_uuidv4() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { value: uuidv4() }; }".to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        let value = result.output["value"].as_str().unwrap();
        assert!(Uuid::parse_str(value).is_ok());
    }

    #[tokio::test]
    async fn test_builtin_btoa_atob() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { value: atob(btoa('hello')) }; }"
                    .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert_eq!(result.output["value"], json!("hello"));
    }

    #[tokio::test]
    async fn test_builtin_datetime_now() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { value: datetime.now() }; }"
                    .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        let value = result.output["value"].as_f64().unwrap();
        assert!(value > 1_500_000_000_000.0);
    }

    #[tokio::test]
    async fn test_builtin_datetime_format() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { value: datetime.format() }; }"
                    .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        let value = result.output["value"].as_str().unwrap();
        let re = Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$").unwrap();
        assert!(re.is_match(value));
    }

    #[tokio::test]
    async fn test_builtin_crypto_md5() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { value: crypto.md5('hello') }; }"
                    .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert_eq!(
            result.output["value"],
            json!("5d41402abc4b2a76b9719d911017c592")
        );
    }

    #[tokio::test]
    async fn test_builtin_crypto_sha256() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { value: crypto.sha256('hello') }; }"
                    .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert_eq!(
            result.output["value"],
            json!("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
    }

    #[tokio::test]
    async fn test_builtin_crypto_hmac() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { value: crypto.hmacSha256('secret', 'hello') }; }"
                    .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
        mac.update(b"hello");
        let expected = hex::encode(mac.finalize().into_bytes());

        assert_eq!(result.output["value"], json!(expected));
    }

    #[tokio::test]
    async fn test_builtin_crypto_aes() {
        let sandbox = default_sandbox();
        let code = r#"function main(inputs) {
            var key = "000102030405060708090a0b0c0d0e0f000102030405060708090a0b0c0d0e0f";
            var enc = crypto.aesEncrypt("hello", key);
            var dec = crypto.aesDecrypt(enc.ciphertext, key, enc.iv);
            return { value: dec };
        }"#;
        let result = sandbox
            .execute(SandboxRequest {
                code: code.to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert_eq!(result.output["value"], json!("hello"));
    }

    #[tokio::test]
    async fn test_builtin_random_int() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { value: randomInt(1, 10) }; }"
                    .to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        let value = result.output["value"].as_i64().unwrap();
        assert!(value >= 1 && value <= 10);
    }

    #[tokio::test]
    async fn test_builtin_random_bytes() {
        let sandbox = default_sandbox();
        let result = sandbox
            .execute(SandboxRequest {
                code: "function main(inputs) { return { value: randomBytes(16) }; }".to_string(),
                language: CodeLanguage::JavaScript,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        let value = result.output["value"].as_str().unwrap();
        assert_eq!(value.len(), 32);
        let re = Regex::new(r"^[0-9a-f]+$").unwrap();
        assert!(re.is_match(value));
    }
}
