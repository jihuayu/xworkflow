# xworkflow 测试覆盖设计文档

## 1. 现状分析

### 1.1 现有单元测试（`#[cfg(test)]` 模块）

| 模块 | 文件 | 测试数量 | 覆盖内容 |
|------|------|----------|----------|
| evaluator | `condition.rs` | 18 | 比较运算符、AND/OR 逻辑、类型转换 |
| core | `variable_pool.rs` | 12 | 基本读写、set_node_outputs、Segment 转换、Stream、append |
| core | `dispatcher.rs` | 有 | 调度器基础流程 |
| core | `debug.rs` | 1 | 基础调试功能 |
| graph | `builder.rs` | 2 | 简单图/分支图构建 |
| graph | `types.rs` | 4 | 节点就绪判断、边处理 |
| dsl | `parser.rs` | 3 | YAML/JSON 解析、无效输入 |
| dsl | `validator.rs` | 4 | 基础 schema 校验 |
| dsl/validation | `mod.rs` | 3 | 解析错误、环检测、Start 变量类型 |
| nodes | `control_flow.rs` | 有 | 控制流节点 |
| nodes | `data_transform.rs` | 5+ | 模板变换、变量聚合、变量赋值、Code JS |
| nodes | `subgraph.rs` | 有 | 子图基础 |
| nodes | `subgraph_nodes.rs` | 有 | 迭代/循环节点 |
| template | `engine.rs` | 7 | Jinja2 渲染 |
| sandbox | `builtin.rs` | 有 | 内置沙箱 |
| sandbox | `js_builtins.rs` | 7 | JS 内置函数 |
| sandbox | `wasm_sandbox.rs` | 有 | WASM 沙箱 |
| sandbox | `manager.rs` | 有 | 沙箱管理器 |
| scheduler | `scheduler.rs` | 有 | WorkflowRunner 基础/分支 |
| llm | `executor.rs` | 有 | LLM 节点执行器 |
| llm/provider | `openai.rs` | 有 | OpenAI provider |
| llm/provider | `wasm_provider.rs` | 1 | WASM LLM provider |
| security | `governor.rs` | 有 | 资源调控器 |
| security | `network.rs` | 2 | 网络策略 |
| security | `sandbox.rs` | 2 | 安全沙箱 |

### 1.2 现有集成/E2E 测试

| 类型 | 文件 | 用例数 |
|------|------|--------|
| E2E 常规 | `tests/integration_tests.rs` | 74 个 case (001-074) |
| E2E 调试 | `tests/integration_tests.rs` | 7 个 case (050-056) |
| E2E 插件 | `tests/integration_tests.rs` | 5 个 case (plugin-system feature) |
| 插件系统 | `tests/plugin_system_tests.rs` | 6 个测试 |
| 插件注册表 | `tests/plugin_system_registry.rs` | 2 个测试 |

### 1.3 现有 E2E 覆盖场景

**已覆盖：**
- 简单直通 (start → end)
- If-Else 分支（true/false/multi-case/or/and/null/empty/in/string）
- 模板变换（单变量/多变量/Jinja2 条件/Jinja2 循环）
- Code 节点（JS 基础/字符串操作/JSON 输出/布尔返回/WASM）
- 变量聚合器（含 fallback）
- 链式管道
- Max steps 超限
- 多输入
- 部分匹配
- 系统变量
- 伪时间/伪ID
- HTTP 请求（GET/POST/PUT/DELETE/PATCH/Header/Auth/Error）
- JS 内置函数
- WASM Code 节点
- LLM 节点（基础/Stream 全系列 057-074）
- 错误恢复 (049)
- 调试模式（断点/步进/变量修改/中止/动态断点）
- 插件系统（Host Node/Bootstrap Sandbox/LLM Provider/Hook）
- 空工作流

---

## 2. 缺失单元测试分析与补充方案

### 2.1 `core/event_bus.rs` — GraphEngineEvent（无任何测试）

**当前状态：** 0 个测试。`to_json()` 方法有 20+ 个分支，全部未覆盖。

**建议测试：**

```
UT-EVT-001: GraphRunStarted.to_json() 正确序列化
UT-EVT-002: GraphRunSucceeded.to_json() 包含 outputs
UT-EVT-003: GraphRunFailed.to_json() 包含 error 和 exceptions_count
UT-EVT-004: NodeRunStarted.to_json() 包含所有字段
UT-EVT-005: NodeRunSucceeded.to_json() 包含 outputs
UT-EVT-006: NodeRunFailed.to_json() 包含 error_type 和 detail
UT-EVT-007: NodeRunStreamChunk.to_json() 包含 chunk/selector/is_final
UT-EVT-008: NodeRunRetry.to_json() 包含 retry_index
UT-EVT-009: IterationStarted/Next/Succeeded/Failed.to_json() 各字段正确
UT-EVT-010: LoopStarted/Next/Succeeded/Failed.to_json() 各字段正确
UT-EVT-011: PluginLoaded/Unloaded/Event/Error.to_json() 各字段正确
UT-EVT-012: DebugPaused/Resumed/BreakpointChanged/VariableSnapshot.to_json()
UT-EVT-013: ErrorHandler 系列事件序列化
UT-EVT-014: GraphRunAborted.to_json() 包含 reason/outputs
UT-EVT-015: GraphRunPartialSucceeded.to_json() 包含 exceptions_count
```

### 2.2 `core/runtime_context.rs` — TimeProvider / IdGenerator（无任何测试）

**当前状态：** 0 个测试。Fake/Real 实现均无测试。

**建议测试：**

```
UT-CTX-001: RealTimeProvider::now_timestamp() 返回合理 UNIX 时间戳
UT-CTX-002: RealTimeProvider::now_millis() > now_timestamp() * 1000 - 1000
UT-CTX-003: RealTimeProvider::elapsed_secs() 对过去时间返回正数
UT-CTX-004: RealTimeProvider::elapsed_secs() 对未来时间返回 0
UT-CTX-005: FakeTimeProvider 返回固定时间戳
UT-CTX-006: FakeTimeProvider::now_millis() = fixed * 1000
UT-CTX-007: FakeTimeProvider::elapsed_secs() 正确计算差值
UT-CTX-008: FakeIdGenerator 递增 ID: prefix-0, prefix-1, prefix-2
UT-CTX-009: FakeIdGenerator 多线程安全性（并发调用不重复）
UT-CTX-010: RealIdGenerator 产生 UUID v4 格式
UT-CTX-011: RuntimeContext::default() 各字段初始状态
UT-CTX-012: RuntimeContext::with_event_tx() 正确设置
UT-CTX-013: RuntimeContext::with_node_executor_registry() 正确设置
```

### 2.3 `core/sub_graph_runner.rs` — 子图执行（无任何测试）

**当前状态：** 0 个测试。`inject_scope_vars`、`build_sub_graph`、`resolve_node_type` 等函数未覆盖。

**建议测试：**

```
UT-SGR-001: build_sub_graph() 正常构建含 start+end 的子图
UT-SGR-002: build_sub_graph() 缺少 start 节点返回 NoStartNode
UT-SGR-003: build_sub_graph() 缺少 end 节点返回 NoEndNode
UT-SGR-004: build_sub_graph() 无效边返回 InvalidEdge
UT-SGR-005: inject_scope_vars() 正常注入变量到 pool
UT-SGR-006: inject_scope_vars() 非法 selector key 返回 ScopeError
UT-SGR-007: resolve_node_type() 从 node_type 字段解析
UT-SGR-008: resolve_node_type() 从 data.type 字段解析
UT-SGR-009: resolve_node_type() 缺少类型返回 None
UT-SGR-010: map_node_type_alias("template") => "template-transform"
UT-SGR-011: map_node_type_alias("code") => "code"（非别名不变）
UT-SGR-012: build_sub_graph() 边 id 为空时自动生成 "edge_N"
```

### 2.4 `nodes/executor.rs` — NodeExecutorRegistry（无任何测试）

**当前状态：** 0 个测试。注册、获取、懒初始化等逻辑未覆盖。

**建议测试：**

```
UT-REG-001: NodeExecutorRegistry::empty() 不含任何 executor
UT-REG-002: NodeExecutorRegistry::new() 包含 start/end/answer/if-else 等内置
UT-REG-003: register() 后 get() 返回正确 executor
UT-REG-004: get() 对未注册类型返回 None
UT-REG-005: register_lazy() 延迟初始化：首次 get 时才调用工厂
UT-REG-006: register_lazy() 第二次 get 不再调用工厂
UT-REG-007: register() 覆盖已有 executor
UT-REG-008: StubExecutor 返回 Exception 状态和 not_implemented 错误
UT-REG-009: set_llm_provider_registry() 注册 "llm" executor
UT-REG-010: apply_plugin_executors() 批量注册插件 executor
```

### 2.5 `nodes/utils.rs` — selector 解析（无任何测试）

**当前状态：** 0 个测试。两个公开函数是简单委托，但核心输入解析逻辑应覆盖。

**建议测试：**

```
UT-UTIL-001: selector_from_value(["node1", "output"]) => 正确 Selector
UT-UTIL-002: selector_from_value([]) => None
UT-UTIL-003: selector_from_value(非数组) => None
UT-UTIL-004: selector_from_str("node1.output") => 正确 Selector
UT-UTIL-005: selector_from_str("") => None
UT-UTIL-006: selector_from_str("single") => None 或单元素 Selector
```

### 2.6 `dsl/schema.rs` — NodeOutputs / NodeType（无任何测试）

**当前状态：** 0 个测试。NodeOutputs 的 ready/streams/into_parts、NodeType::execution_type 等未覆盖。

**建议测试：**

```
UT-SCH-001: NodeOutputs::Sync::ready() 返回内部 HashMap
UT-SCH-002: NodeOutputs::Stream::ready() 返回 ready 部分
UT-SCH-003: NodeOutputs::Sync::streams() 返回 None
UT-SCH-004: NodeOutputs::Stream::streams() 返回 Some
UT-SCH-005: NodeOutputs::into_parts() Sync 返回空 streams
UT-SCH-006: NodeOutputs::into_parts() Stream 拆分正确
UT-SCH-007: NodeType::Start.execution_type() => Root
UT-SCH-008: NodeType::End.execution_type() => Response
UT-SCH-009: NodeType::IfElse.execution_type() => Branch
UT-SCH-010: NodeType::Iteration.execution_type() => Container
UT-SCH-011: NodeType::Code.execution_type() => Executable
UT-SCH-012: NodeType::Display 输出 kebab-case
UT-SCH-013: ComparisonOperator serde 别名反序列化（"=", "≠", ">", "<" 等）
UT-SCH-014: EdgeHandle::Default / Branch 序列化
UT-SCH-015: NodeRunResult::default() 字段初始值
UT-SCH-016: WorkflowSchema 反序列化含 environment_variables / conversation_variables
```

### 2.7 `dsl/validation/layer1_structure.rs` — 结构校验（无直接测试）

**当前状态：** 仅通过 `validation/mod.rs` 的 3 个集成测试间接覆盖。各错误码独立覆盖不足。

**建议测试：**

```
UT-L1-001: 不支持的 DSL 版本 => E002
UT-L1-002: 无节点 => E003
UT-L1-003: 无 start 节点 => E004
UT-L1-004: 多个 start 节点 => E005
UT-L1-005: 无 end/answer 节点 => E006
UT-L1-006: 重复 node id => E007
UT-L1-007: 空 node id => E008
UT-L1-008: 未知 node type => E009
UT-L1-009: edge source 不存在 => E010
UT-L1-010: edge target 不存在 => E011
UT-L1-011: edge source == target => E012
UT-L1-012: 重复 edge => E013
UT-L1-013: 空 title => W001 (warning)
UT-L1-014: 正常 schema 无错误
```

### 2.8 `dsl/validation/layer2_topology.rs` — 拓扑校验（无直接测试）

**建议测试：**

```
UT-L2-001: start 节点有入边 => E104
UT-L2-002: 不可达节点 => E102
UT-L2-003: start 到 end 无路径 => E103
UT-L2-004: end 节点有出边 => W101
UT-L2-005: 孤立边 => W102
UT-L2-006: 环检测 => E101
UT-L2-007: 复杂 DAG 正常通过
UT-L2-008: BFS 层级正确赋值（用于语义校验）
```

### 2.9 `dsl/validation/layer3_semantic.rs` — 语义校验（无直接测试）

**建议测试：**

```
UT-L3-001: start 节点无效变量类型 => E201
UT-L3-002: end 输出 selector 为空 => E202
UT-L3-003: answer 模板为空 => E203
UT-L3-004: if-else cases 为空 => E204
UT-L3-005: if-else case conditions 为空 => E205
UT-L3-006: if-else 重复 case_id => E206
UT-L3-007: code 为空 => E207
UT-L3-008: code 不支持的语言 => E208
UT-L3-009: http-request url 为空 => E209
UT-L3-010: template 为空 => E210
UT-L3-011: iteration sub_graph 为空 => E211
UT-L3-012: iteration iterator_selector 为空 => E212
UT-L3-013: loop sub_graph 为空 => E213
UT-L3-014: loop break condition 为空 => E214
UT-L3-015: variable-aggregator variables 为空 => E215
UT-L3-016: list-operator 缺少 operation => E216
UT-L3-017: 节点配置解析失败 => E217
UT-L3-018: 分支节点缺少 false 边 => E301
UT-L3-019: 分支节点 case 缺少对应边 => E302
UT-L3-020: 非分支节点有 source_handle => E303
UT-L3-021: 分支边未知 source_handle => E304
UT-L3-022: selector 太短 => E401
UT-L3-023: selector 引用未知节点 => E402
UT-L3-024: selector 引用下游节点 => E403
UT-L3-025: selector 引用不可达节点 => W401
UT-L3-026: Jinja2 语法错误 => E501
UT-L3-027: Dify 模板 selector 格式错误 => E502
UT-L3-028: Dify 模板引用未知节点 => W502
UT-L3-029: stub 节点类型 => W202
UT-L3-030: python3 语言警告 => W201
```

### 2.10 `error/node_error.rs` — NodeError（无任何测试）

**建议测试：**

```
UT-ERR-001: NodeError::ConfigError 格式化消息正确
UT-ERR-002: NodeError::with_context() 封装 WithContext
UT-ERR-003: NodeError::error_context() 返回 Some (WithContext)
UT-ERR-004: NodeError::error_context() 返回 None (普通变体)
UT-ERR-005: NodeError::is_retryable() Timeout => true
UT-ERR-006: NodeError::is_retryable() HttpError => true
UT-ERR-007: NodeError::is_retryable() ConfigError => false
UT-ERR-008: NodeError::is_retryable() WithContext + Retryable => true
UT-ERR-009: NodeError::error_code() 各变体返回正确字符串
UT-ERR-010: NodeError::to_structured_json() 无 context
UT-ERR-011: NodeError::to_structured_json() 有 context
UT-ERR-012: From<serde_json::Error> 转换
```

### 2.11 `error/error_context.rs` — ErrorContext（无任何测试）

**建议测试：**

```
UT-ECTX-001: ErrorContext::non_retryable() 字段正确
UT-ECTX-002: ErrorContext::retryable() 字段正确
UT-ECTX-003: ErrorContext::with_retry_after() 设置 retry_after_secs
UT-ECTX-004: ErrorContext::with_http_status() 设置 http_status
UT-ECTX-005: ErrorContext serde 序列化/反序列化 round-trip
UT-ECTX-006: ErrorCode serde rename_all snake_case
```

### 2.12 `security/resource_group.rs` — ResourceGroup / ResourceQuota（无任何测试）

**建议测试：**

```
UT-SGRP-001: ResourceQuota::default() 各字段合理默认值
UT-SGRP-002: ResourceGroup 构造并访问各字段
UT-SGRP-003: ResourceQuota serde 序列化/反序列化 round-trip
```

### 2.13 `security/policy.rs` — SecurityPolicy（无任何测试）

**建议测试：**

```
UT-SPOL-001: SecurityPolicy::permissive() 无任何限制
UT-SPOL-002: SecurityPolicy::standard() 包含 network/template/dsl_validation
UT-SPOL-003: SecurityPolicy::strict() 网络策略为 AllowList
UT-SPOL-004: default_node_limits() 包含 code/http/llm/template 四种
UT-SPOL-005: strict_node_limits() 时间限制比 default 更短
UT-SPOL-006: NodeResourceLimits::for_code_node() 字段正确
UT-SPOL-007: SecurityLevel default => Standard
```

### 2.14 `security/credential.rs` — CredentialProvider（无任何测试）

**建议测试：**

```
UT-SCRE-001: CredentialError::NotFound 格式化正确
UT-SCRE-002: CredentialError::AccessDenied 格式化正确
UT-SCRE-003: CredentialError::ProviderError 格式化正确
```

### 2.15 `security/audit.rs` — AuditLogger（无任何测试）

**建议测试：**

```
UT-SAUD-001: TracingAuditLogger::log_event() Critical 不 panic
UT-SAUD-002: TracingAuditLogger::log_event() Warning 不 panic
UT-SAUD-003: TracingAuditLogger::log_event() Info 不 panic
UT-SAUD-004: AuditLogger::log_events() 默认实现逐条调用
UT-SAUD-005: SecurityEvent 构造并 Serialize 正确
UT-SAUD-006: SecurityEventType 各变体 Debug 输出
```

### 2.16 `security/validation.rs` — 配置结构（无任何测试）

**建议测试：**

```
UT-SVAL-001: DslValidationConfig::default() 各字段合理
UT-SVAL-002: TemplateSafetyConfig::default() 各字段合理
UT-SVAL-003: SelectorValidation::default() 包含 sys/env 前缀
UT-SVAL-004: DslValidationConfig serde round-trip
UT-SVAL-005: TemplateSafetyConfig serde round-trip
```

### 2.17 `plugin_system/*` — 插件系统核心（大部分无测试）

**涉及文件：** `registry.rs`, `context.rs`, `loader.rs`, `config.rs`, `hooks.rs`, `traits.rs`, `extensions.rs`, `macros.rs`

**建议测试：**

```
UT-PS-001: PluginRegistry::new() 空注册表
UT-PS-002: PluginContext::register_node_executor() 成功注册
UT-PS-003: PluginContext::register_node_executor() 重复注册返回错误
UT-PS-004: PluginContext::register_sandbox() 成功
UT-PS-005: PluginContext::register_llm_provider() 成功
UT-PS-006: PluginContext::register_hook() 成功
UT-PS-007: PluginContext::register_plugin_loader() 成功
UT-PS-008: PluginSystemConfig::default() 字段正确
UT-PS-009: HookPoint 枚举序列化
UT-PS-010: PluginMetadata 构造与访问
UT-PS-011: PluginLoadSource 参数正确
UT-PS-012: PluginError 各变体格式化
```

---

## 3. 缺失 E2E 测试分析与补充方案

### 3.1 错误处理与边界场景

**当前缺失：** 现有 E2E 主要覆盖 happy path，错误场景仅有 013（max_steps）和 049（error_handler_recover）。

```
E2E-ERR-001: 节点执行超时（timeout_secs 配置）
E2E-ERR-002: Code 节点 JS 语法错误 => 失败
E2E-ERR-003: Code 节点 JS 运行时异常（throw Error）=> 失败
E2E-ERR-004: Code 节点无 main 函数 => 失败
E2E-ERR-005: Code 节点返回非对象 => 失败
E2E-ERR-006: 模板变换引用不存在的变量 => 输出空串或失败
E2E-ERR-007: If-Else 引用不存在的变量（应走 false 分支）
E2E-ERR-008: HTTP 请求连接超时 => 失败
E2E-ERR-009: HTTP 请求 URL 无效 => 失败
E2E-ERR-010: 变量聚合器所有来源均为 null => 输出 null
E2E-ERR-011: End 节点引用不存在的 selector => 输出 null
E2E-ERR-012: Start 节点缺少必需输入 => 失败
```

### 3.2 错误策略（Error Strategy）

**当前缺失：** `error_strategy` 配置（fail-branch / default-value）无任何 E2E 覆盖。

```
E2E-ES-001: error_strategy=fail-branch，节点失败走 fail 分支
E2E-ES-002: error_strategy=default-value，节点失败使用默认值继续
E2E-ES-003: error_strategy=none，节点失败直接终止工作流
```

### 3.3 重试机制（Retry）

**当前缺失：** `retry_config` 无任何 E2E 覆盖。

```
E2E-RET-001: retry_config: max_retries=2, 第一次失败第二次成功 => 成功
E2E-RET-002: retry_config: max_retries=1, 全部失败 => 最终失败
E2E-RET-003: retry_config: backoff_strategy=exponential 验证重试间隔
E2E-RET-004: retry_config: retry_on_retryable_only=true, 非重试错误不重试
```

### 3.4 迭代节点（Iteration）

**当前缺失：** 无 iteration 节点的 E2E 测试。

```
E2E-ITER-001: 简单列表迭代，收集每个元素处理结果
E2E-ITER-002: 空列表迭代 => 空结果
E2E-ITER-003: 迭代中子节点失败 + error_handle_mode=terminated => 整体失败
E2E-ITER-004: 迭代中子节点失败 + error_handle_mode=remove_abnormal => 移除异常项
E2E-ITER-005: 迭代中子节点失败 + error_handle_mode=continue_on_error => 继续
E2E-ITER-006: 并行迭代 (is_parallel=true)
E2E-ITER-007: 迭代嵌套模板变换
E2E-ITER-008: 大列表迭代（100个元素）性能可控
```

### 3.5 循环节点（Loop）

**当前缺失：** 无 loop 节点的 E2E 测试。

```
E2E-LOOP-001: 简单条件循环（循环 N 次后满足条件退出）
E2E-LOOP-002: 循环体内含 if-else 分支
E2E-LOOP-003: 循环达到 max_iterations 强制退出
E2E-LOOP-004: 循环 break_condition 首次即满足 => 不执行循环体
E2E-LOOP-005: 循环中使用 assigner 节点更新变量
E2E-LOOP-006: 循环体内嵌套迭代
```

### 3.6 List Operator 节点

**当前缺失：** 无 list-operator 的 E2E 测试。

```
E2E-LIST-001: list-operator filter 操作
E2E-LIST-002: list-operator sort 操作
E2E-LIST-003: list-operator slice 操作
E2E-LIST-004: list-operator 空列表输入
E2E-LIST-005: list-operator 与 iteration 联合使用
```

### 3.7 Variable Assigner 节点

**当前缺失：** 无 assigner 节点的 E2E 测试（仅有 071 的 stream 场景）。

```
E2E-ASGN-001: assigner overwrite 模式
E2E-ASGN-002: assigner append 模式（追加到数组）
E2E-ASGN-003: assigner clear 模式
E2E-ASGN-004: assigner 使用静态 value 而非 input_variable_selector
E2E-ASGN-005: assigner 写入 conversation_variable
```

### 3.8 Conversation Variables

**当前缺失：** conversation_variables 在工作流间传递的场景。

```
E2E-CONV-001: 读取 conversation_variable 默认值
E2E-CONV-002: assigner 更新 conversation_variable，后续节点读取更新值
E2E-CONV-003: conversation_variable 类型为 number
E2E-CONV-004: conversation_variable 类型为 array
```

### 3.9 Environment Variables

**当前缺失：** environment_variables 使用场景仅在 state.json 中设置，无 E2E 直接验证。

```
E2E-ENV-001: 模板中使用 env.VAR_NAME 引用环境变量
E2E-ENV-002: if-else 条件中引用环境变量
E2E-ENV-003: HTTP url 中使用环境变量
```

### 3.10 工作流级错误处理器（Error Handler）

**当前缺失：** error_handler 仅有 049 一个 recover 场景。

```
E2E-EH-001: error_handler mode=recover，子图恢复成功
E2E-EH-002: error_handler mode=notify，记录错误但不恢复
E2E-EH-003: error_handler 子图自身也失败 => 双重失败
E2E-EH-004: error_handler 访问错误信息变量
```

### 3.11 并发与调度

**当前缺失：** 多分支并行执行场景。

```
E2E-CONC-001: 钻石形 DAG（start → A+B → merge → end）并行执行
E2E-CONC-002: 多个独立分支最终聚合
E2E-CONC-003: 分支中一个超时另一个成功
```

### 3.12 DSL 校验 E2E

**当前缺失：** 025（空工作流）是唯一的校验 E2E，更多校验场景应覆盖。

```
E2E-VAL-001: DSL 版本不支持 => 启动失败
E2E-VAL-002: 无 start 节点 => 启动失败
E2E-VAL-003: 有环的 DSL => 启动失败
E2E-VAL-004: edge 引用不存在节点 => 启动失败
E2E-VAL-005: 重复 node id => 启动失败
```

### 3.13 安全功能 E2E（security feature）

**当前缺失：** security feature 完全无 E2E 测试。

```
E2E-SEC-001: 网络策略 block_private_ips，HTTP 请求被拦截
E2E-SEC-002: 网络策略 AllowList，请求非白名单域名被拦截
E2E-SEC-003: 资源调控器配额超限
E2E-SEC-004: 节点输出大小超限 => OutputTooLarge
E2E-SEC-005: 模板安全配置限制模板长度
```

### 3.14 Stream 相关补充

**当前已有 057-074 覆盖较全，但缺少错误场景。**

```
E2E-STR-001: LLM stream 中途报错 => 工作流失败
E2E-STR-002: Stream 传播到 answer 节点，部分输出
E2E-STR-003: 多个 stream 同时活跃
```

### 3.15 WASM 沙箱扩展

**当前仅有 046 一个基础 case。**

```
E2E-WASM-001: WASM code 节点带输入变量
E2E-WASM-002: WASM code 节点返回复杂 JSON
E2E-WASM-003: WASM code 节点执行失败（trap）
E2E-WASM-004: WASM code 节点超时（fuel 耗尽）
```

---

## 4. 优先级排序

### P0 — 必须补充（核心逻辑无覆盖，回归风险高）

| ID | 类型 | 模块 | 说明 |
|----|------|------|------|
| UT-REG-* | 单元 | nodes/executor | Registry 是所有节点路由核心 |
| UT-L1-* | 单元 | dsl/validation/layer1 | 结构校验是安全门户 |
| UT-L2-* | 单元 | dsl/validation/layer2 | 拓扑校验防止非法 DAG |
| UT-L3-* | 单元 | dsl/validation/layer3 | 语义校验防止运行时崩溃 |
| UT-ERR-* | 单元 | error/node_error | 错误分类影响重试/恢复 |
| E2E-ITER-* | E2E | 迭代节点 | 核心控制流无覆盖 |
| E2E-LOOP-* | E2E | 循环节点 | 核心控制流无覆盖 |
| E2E-ERR-* | E2E | 错误场景 | 错误 path 覆盖不足 |
| E2E-ES-* | E2E | 错误策略 | fail-branch/default-value 无覆盖 |

### P1 — 应该补充（重要但非立即阻断）

| ID | 类型 | 模块 | 说明 |
|----|------|------|------|
| UT-EVT-* | 单元 | event_bus | 事件 JSON 序列化正确性 |
| UT-CTX-* | 单元 | runtime_context | 时间/ID 生成是确定性前提 |
| UT-SGR-* | 单元 | sub_graph_runner | 子图构建是迭代/循环的基础 |
| UT-SCH-* | 单元 | dsl/schema | 数据类型正确性 |
| E2E-ASGN-* | E2E | assigner | 变量写入模式 |
| E2E-CONV-* | E2E | conversation vars | 状态持久化 |
| E2E-LIST-* | E2E | list-operator | 列表操作 |
| E2E-RET-* | E2E | 重试机制 | 容错能力 |
| E2E-CONC-* | E2E | 并发/钻石 DAG | 并行调度正确性 |

### P2 — 可以补充（增强信心）

| ID | 类型 | 模块 | 说明 |
|----|------|------|------|
| UT-SGRP/SPOL/SCRE/SAUD/SVAL-* | 单元 | security/* | 安全策略配置 |
| UT-PS-* | 单元 | plugin_system | 插件系统核心 |
| UT-ECTX-* | 单元 | error_context | 结构化错误 |
| UT-UTIL-* | 单元 | nodes/utils | selector 解析 |
| E2E-SEC-* | E2E | 安全功能 | security feature |
| E2E-EH-* | E2E | 错误处理器 | error_handler 扩展场景 |
| E2E-VAL-* | E2E | DSL 校验 | 启动时拦截 |
| E2E-WASM-* | E2E | WASM 扩展 | 沙箱边界 |
| E2E-STR-* | E2E | Stream 错误 | 流式异常 |
| E2E-ENV-* | E2E | 环境变量 | env 引用 |

---

## 5. 统计总结

| 类别 | 现有 | 建议新增 | 总计 |
|------|------|----------|------|
| 单元测试 | ~46 个 #[test] + ~25 文件有 cfg(test) | ~170 个 | ~216 |
| 集成测试 | 8 个 (plugin_system) | — | 8 |
| E2E 测试 | ~81 个 case | ~72 个 | ~153 |
| 总计 | ~135 | ~242 | ~377 |

### 无测试文件清单

以下 `.rs` 文件完全无单元测试（0 个 `#[test]`）：

1. `src/core/event_bus.rs`
2. `src/core/runtime_context.rs`
3. `src/core/sub_graph_runner.rs`
4. `src/core/mod.rs`
5. `src/nodes/executor.rs`
6. `src/nodes/utils.rs`
7. `src/nodes/mod.rs`
8. `src/dsl/schema.rs`
9. `src/dsl/mod.rs`
10. `src/dsl/validation/known_types.rs`
11. `src/dsl/validation/layer1_structure.rs`
12. `src/dsl/validation/layer2_topology.rs`
13. `src/dsl/validation/layer3_semantic.rs`
14. `src/dsl/validation/types.rs`
15. `src/error/workflow_error.rs`
16. `src/error/node_error.rs`
17. `src/error/error_context.rs`
18. `src/error/mod.rs`
19. `src/security/resource_group.rs`
20. `src/security/policy.rs`
21. `src/security/credential.rs`
22. `src/security/audit.rs`
23. `src/security/validation.rs`
24. `src/security/mod.rs`
25. `src/plugin_system/registry.rs`
26. `src/plugin_system/context.rs`
27. `src/plugin_system/loader.rs`
28. `src/plugin_system/config.rs`
29. `src/plugin_system/hooks.rs`
30. `src/plugin_system/traits.rs`
31. `src/plugin_system/extensions.rs`
32. `src/plugin_system/macros.rs`
33. `src/plugin_system/mod.rs`
34. `src/plugin_system/loaders/host_loader.rs`
35. `src/plugin_system/loaders/dll_loader.rs`
36. `src/plugin_system/loaders/mod.rs`
37. `src/plugin_system/builtins/sandbox_js.rs`
38. `src/plugin_system/builtins/sandbox_wasm.rs`
39. `src/plugin_system/builtins/template_jinja.rs`
40. `src/plugin_system/builtins/wasm_bootstrap.rs`
41. `src/plugin_system/builtins/mod.rs`
42. `src/sandbox/types.rs`
43. `src/sandbox/error.rs`
44. `src/sandbox/mod.rs`
45. `src/llm/types.rs`
46. `src/llm/error.rs`
47. `src/llm/mod.rs`
48. `src/llm/provider/mod.rs`
49. `src/plugin/manifest.rs`
50. `src/plugin/host_functions.rs`
51. `src/plugin/manager.rs`
52. `src/plugin/mod.rs`
53. `src/plugin/error.rs`
54. `src/plugin/runtime.rs`
55. `src/template/mod.rs`
56. `src/lib.rs`
57. `src/main.rs`

---

## 6. 实施建议

1. **优先处理 P0：** 从 `dsl/validation/layer1-3`、`nodes/executor`、`error/node_error` 开始补充单元测试，这些是纯函数/结构体，不需要 async runtime，编写成本低。

2. **E2E 框架已成熟：** 现有的 case-based E2E 框架（workflow.json + in.json + out.json + state.json）设计优秀，新增 case 只需添加目录，无需修改 runner 代码。Iteration/Loop/Assigner/Error Strategy 等 E2E 用例可以批量添加。

3. **测试隔离：** 建议为 `security` feature 的测试使用独立的 `#[cfg(feature = "security")]` 守卫，避免在默认编译时引入依赖。

4. **Mock 策略：**
   - HTTP 测试已有 mockito 支持
   - LLM 测试已有 mock server 支持
   - 重试测试需要可控的失败计数器（建议在 state.json 中扩展 mock_server 支持失败次数）

5. **CI 集成：** 建议在 CI 中分别运行：
   - `cargo test` — 默认 feature 的单元+E2E
   - `cargo test --features plugin-system` — 插件系统测试
   - `cargo test --features security` — 安全功能测试
   - `cargo test --all-features` — 全量测试
