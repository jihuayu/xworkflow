# 工作流编译（Workflow Compilation）设计文档

## 1. 背景与动机

### 1.1 问题分析

当前每次执行工作流时，`WorkflowRunnerBuilder::run()` 都会重复执行以下 **完整流水线**：

```
DSL 文本 (YAML/JSON)
    ↓ [1] parse_dsl()           — 字符串解析 + serde 反序列化
    ↓ [2] validate_schema()      — 三层验证（结构/拓扑/语义）
    ↓ [3] build_graph()          — 构建 Graph (HashMap 分配、邻接表)
    ↓ [4] collect_*_types()      — 提取 start/conversation 变量类型映射
    ↓ [5] 初始化 VariablePool    — 写入 sys/env/conversation/user 变量
    ↓ [6] 构建 NodeExecutorRegistry
    ↓ [7] 创建 Dispatcher → 执行
```

在同一个工作流被多次运行的场景下（例如 API 服务、批处理、聊天对话），步骤 [1]-[4] 的输出是 **确定性的** ——只要 DSL 内容不变，这些步骤的结果完全相同。这些步骤包含：

| 步骤 | 开销来源 | 重复浪费 |
|------|---------|---------|
| DSL 解析 | YAML/JSON parser、serde 反序列化、大量 String/Vec/HashMap 分配 | 高 |
| 三层验证 | DFS 环检测、BFS 可达性、语义检查、模板语法编译 | 高 |
| Graph 构建 | HashMap 分配、邻接表构建、config JSON 组装 | 中 |
| 类型提取 | 遍历 schema 提取 start/conversation 变量类型 | 低 |

此外，节点执行器在每次执行时都从 `Value`（JSON）反序列化各自的配置（如 `IfElseNodeData`、`CodeNodeData`），这也是可缓存的。

### 1.2 目标

引入 **编译（Compilation）** 阶段，将 DSL 文本转化为一个 **可复用的编译产物**（`CompiledWorkflow`），使得多次执行同一工作流时只需要执行步骤 [5]-[7]（Per-Run 阶段），跳过所有解析、验证和图构建。

### 1.3 类比

```
源代码  →  编译  →  可执行文件  →  多次运行（不同输入）
DSL 文本 →  compile →  CompiledWorkflow →  多次 run（不同 user_inputs）
```

## 2. 设计概览

### 2.1 核心思路

将当前 `WorkflowRunnerBuilder::run()` 中的逻辑拆分为两个阶段：

```
┌──────────────────────────────────────────────────────────────┐
│  Compile Phase（一次性，可缓存）                               │
│                                                              │
│  DSL Text → parse → validate → build_graph                   │
│         → extract type maps → pre-compile templates          │
│         → CompiledWorkflow                                   │
└──────────────────────────────────────────────────────────────┘
                          ↓
                   CompiledWorkflow (Arc, Send+Sync, 可 clone)
                          ↓
┌──────────────────────────────────────────────────────────────┐
│  Execute Phase（每次运行）                                    │
│                                                              │
│  CompiledWorkflow + user_inputs + env/sys/conv vars          │
│         → init VariablePool → clone Graph → Dispatcher.run() │
└──────────────────────────────────────────────────────────────┘
```

### 2.2 数据流对比

**Before（当前）：**
```rust
// 每次运行都要传入 schema，全量重建
let handle = WorkflowRunner::builder(schema)  // schema: WorkflowSchema
    .user_inputs(inputs)
    .run()
    .await?;
```

**After（编译模式）：**
```rust
// 编译一次
let compiled = WorkflowCompiler::compile(dsl_text, DslFormat::Yaml)?;
// 或从 schema 编译
let compiled = WorkflowCompiler::compile_schema(schema)?;

// 多次运行，复用编译产物
let handle1 = compiled.runner()
    .user_inputs(inputs1)
    .run()
    .await?;

let handle2 = compiled.runner()
    .user_inputs(inputs2)
    .run()
    .await?;
```

## 3. 详细设计

### 3.1 CompiledWorkflow 结构

```rust
/// 工作流的编译产物，包含所有从 DSL 预计算的不可变数据。
///
/// 线程安全 (Send + Sync)，可通过 Arc 共享，可 Clone（内部全部 Arc）。
/// 同一 CompiledWorkflow 可并发启动多个执行实例。
#[derive(Clone)]
pub struct CompiledWorkflow {
    /// 编译时间戳
    compiled_at: std::time::Instant,

    /// DSL 内容摘要（用于缓存命中判断）
    content_hash: u64,

    /// 原始 schema（保留用于 error handler sub_graph、事件上下文等）
    schema: Arc<WorkflowSchema>,

    /// 预构建的执行图（不可变模板，每次执行时 clone）
    graph_template: Arc<Graph>,

    /// Start 节点变量的类型映射  { var_name → SegmentType }
    start_var_types: Arc<HashMap<String, SegmentType>>,

    /// Conversation 变量的类型映射  { var_name → SegmentType }
    conversation_var_types: Arc<HashMap<String, SegmentType>>,

    /// Start 节点 ID（从 graph 提取，避免运行时查找）
    start_node_id: Arc<str>,

    /// 验证报告（包含 warnings，调用方可检查）
    validation_report: Arc<ValidationReport>,

    /// 预编译的节点配置（避免运行时重复 JSON 反序列化）
    node_configs: Arc<HashMap<String, CompiledNodeConfig>>,
}
```

### 3.2 CompiledNodeConfig — 预编译的节点配置

当前每个节点执行器在 `execute()` 中都会从 `config: &Value` 反序列化自己需要的配置结构。例如 IfElse 执行器每次都会 `serde_json::from_value::<IfElseNodeData>(config)`。编译阶段可以提前完成这个反序列化：

```rust
/// 预编译的节点配置，避免运行时 JSON 反序列化
pub enum CompiledNodeConfig {
    /// 通用节点：保留原始 JSON config（兜底，用于插件节点等无法预编译的类型）
    Raw(Value),

    /// 预解析的强类型配置（按节点类型枚举）
    Start(StartNodeData),
    End(EndNodeData),
    IfElse(IfElseNodeData),
    Code(CodeNodeData),
    TemplateTransform(TemplateTransformNodeData),
    HttpRequest(HttpRequestNodeData),
    VariableAggregator(VariableAggregatorNodeData),
    VariableAssigner(VariableAssignerNodeData),
    Iteration(IterationNodeData),
    Loop(LoopNodeData),
    Answer(AnswerNodeData),
    ListOperator(ListOperatorNodeData),
    // ... 其他已知类型
}
```

**设计取舍**：是否引入 `CompiledNodeConfig` 需要权衡：
- **优点**：消除运行时反序列化开销，类型安全
- **缺点**：增加编译器与节点类型的耦合，需要随新节点类型同步更新
- **建议**：初始版本只做 `Raw(Value)` 兜底 + 少数热路径节点的预编译（IfElse、Code、TemplateTransform），后续按需扩展

### 3.3 WorkflowCompiler — 编译入口

```rust
pub struct WorkflowCompiler;

impl WorkflowCompiler {
    /// 从 DSL 文本编译
    pub fn compile(
        content: &str,
        format: DslFormat,
    ) -> Result<CompiledWorkflow, WorkflowError> {
        // 1. 解析 DSL
        let schema = parse_dsl(content, format)?;
        Self::compile_schema(schema)
    }

    /// 从已有 schema 编译
    pub fn compile_schema(
        schema: WorkflowSchema,
    ) -> Result<CompiledWorkflow, WorkflowError> {
        // 2. 三层验证
        let report = validate_schema(&schema);
        if !report.is_valid {
            return Err(WorkflowError::ValidationFailed(report));
        }

        // 3. 构建 Graph
        let graph = build_graph(&schema)?;

        // 4. 提取类型映射
        let start_var_types = collect_start_variable_types(&schema);
        let conversation_var_types = collect_conversation_variable_types(&schema);
        let start_node_id = graph.root_node_id.clone();

        // 5. 预编译节点配置
        let node_configs = Self::compile_node_configs(&schema);

        // 6. 计算内容摘要
        let content_hash = Self::hash_schema(&schema);

        Ok(CompiledWorkflow {
            compiled_at: std::time::Instant::now(),
            content_hash,
            schema: Arc::new(schema),
            graph_template: Arc::new(graph),
            start_var_types: Arc::new(start_var_types),
            conversation_var_types: Arc::new(conversation_var_types),
            start_node_id: Arc::from(start_node_id),
            validation_report: Arc::new(report),
            node_configs: Arc::new(node_configs),
        })
    }

    /// 对 schema 计算哈希（用于缓存键）
    fn hash_schema(schema: &WorkflowSchema) -> u64 {
        // 使用 serde 序列化后计算 hash
    }

    /// 预编译各节点配置
    fn compile_node_configs(
        schema: &WorkflowSchema,
    ) -> HashMap<String, CompiledNodeConfig> {
        // 对每个节点尝试反序列化为强类型
        // 失败时回退到 Raw(Value)
    }
}
```

### 3.4 从 CompiledWorkflow 创建 Runner

`CompiledWorkflow` 提供 `runner()` 方法创建 `CompiledWorkflowRunnerBuilder`，其接口与现有 `WorkflowRunnerBuilder` 保持一致，但跳过编译阶段：

```rust
impl CompiledWorkflow {
    /// 创建一个新的执行 builder
    pub fn runner(&self) -> CompiledWorkflowRunnerBuilder {
        CompiledWorkflowRunnerBuilder {
            compiled: self.clone(),  // Arc clone，轻量
            user_inputs: HashMap::new(),
            system_vars: HashMap::new(),
            environment_vars: HashMap::new(),
            conversation_vars: HashMap::new(),
            config: EngineConfig::default(),
            context: RuntimeContext::default(),
            // ... 其他字段同 WorkflowRunnerBuilder
        }
    }

    /// 获取验证报告（包含 warnings）
    pub fn validation_report(&self) -> &ValidationReport {
        &self.validation_report
    }

    /// 获取 DSL 内容哈希
    pub fn content_hash(&self) -> u64 {
        self.content_hash
    }

    /// 获取编译时间
    pub fn compiled_at(&self) -> std::time::Instant {
        self.compiled_at
    }
}
```

### 3.5 CompiledWorkflowRunnerBuilder

```rust
pub struct CompiledWorkflowRunnerBuilder {
    compiled: CompiledWorkflow,
    user_inputs: HashMap<String, Value>,
    system_vars: HashMap<String, Value>,
    environment_vars: HashMap<String, Value>,
    conversation_vars: HashMap<String, Value>,
    config: EngineConfig,
    context: RuntimeContext,
    plugin_gate: Box<dyn SchedulerPluginGate>,
    security_gate: Arc<dyn SchedulerSecurityGate>,
    llm_provider_registry: Option<Arc<LlmProviderRegistry>>,
    debug_config: Option<DebugConfig>,
    collect_events: bool,
}

impl CompiledWorkflowRunnerBuilder {
    // Builder 方法与 WorkflowRunnerBuilder 完全一致
    pub fn user_inputs(mut self, inputs: HashMap<String, Value>) -> Self { ... }
    pub fn system_vars(mut self, vars: HashMap<String, Value>) -> Self { ... }
    // ... 其他 builder 方法

    pub async fn run(self) -> Result<WorkflowHandle, WorkflowError> {
        // ✅ 跳过 parse、validate、build_graph
        // ✅ 直接使用预编译产物

        // 1. Security gate 验证（仍需每次执行，因为依赖运行时上下文）
        let report = self.security_gate
            .validate_schema(&self.compiled.schema, &self.context);
        if !report.is_valid {
            return Err(WorkflowError::ValidationFailed(report));
        }

        // 2. Plugin gate（仍需每次执行）
        // ...

        // 3. Clone graph（从预构建模板 clone，比从 schema 重建快）
        let graph = (*self.compiled.graph_template).clone();

        // 4. 初始化 VariablePool（Per-Run，不可缓存）
        let mut pool = VariablePool::new();
        // 使用预提取的类型映射
        for (k, v) in &self.user_inputs {
            let seg = segment_from_type(v, self.compiled.start_var_types.get(k));
            pool.set(&Selector::new(self.compiled.start_node_id.as_ref(), k), seg);
        }
        // ... sys/env/conversation 同理

        // 5. NodeExecutorRegistry（可复用）
        let mut registry = NodeExecutorRegistry::new();
        // ...

        // 6. 创建 Dispatcher，启动执行
        // 与现有逻辑相同
    }
}
```

### 3.6 安全门与插件门的处理

注意 `security_gate.validate_schema()` 和 `plugin_gate.init_and_extend_validation()` 依赖运行时上下文（SecurityContext、Plugin 配置），**不能**在编译阶段执行。这些验证仍需每次执行时运行。

但可以做以下优化：

```
编译阶段：                    执行阶段：
├─ DSL parse ✅               ├─ security validate（轻量，只检查权限）
├─ 3-layer validate ✅        ├─ plugin extend validate
├─ build_graph ✅             ├─ clone graph（从模板 clone）
├─ extract types ✅           ├─ init pool
└─ compile node configs ✅    ├─ build registry
                              └─ dispatch
```

## 4. Graph Clone 优化

当前 `Graph` 使用 `HashMap<String, GraphNode>` 等字段，`clone()` 涉及大量堆分配。可以进一步优化：

### 4.1 方案 A：Copy-on-Write Graph（推荐）

Graph 的不可变部分（节点配置、邻接表）和可变部分（边遍历状态）分离：

```rust
/// 不可变的图结构（编译产物，Arc 共享）
pub struct GraphTopology {
    pub nodes: HashMap<String, GraphNodeStatic>,  // 不含 state
    pub edges: HashMap<String, GraphEdgeStatic>,  // 不含 state
    pub in_edges: HashMap<String, Vec<String>>,
    pub out_edges: HashMap<String, Vec<String>>,
    pub root_node_id: String,
}

/// 静态节点信息（不含运行时状态）
pub struct GraphNodeStatic {
    pub id: String,
    pub node_type: String,
    pub title: String,
    pub config: Value,
    pub version: String,
    pub error_strategy: Option<ErrorStrategyConfig>,
    pub retry_config: Option<RetryConfig>,
    pub timeout_secs: Option<u64>,
}

/// 每次执行的可变状态（轻量）
pub struct GraphExecutionState {
    pub topology: Arc<GraphTopology>,          // 共享不可变拓扑
    pub node_states: HashMap<String, EdgeTraversalState>,  // 仅状态
    pub edge_states: HashMap<String, EdgeTraversalState>,  // 仅状态
}
```

**优势**：
- `GraphTopology` 通过 Arc 共享，零拷贝
- 每次执行只需创建 `GraphExecutionState`（两个轻量 HashMap，仅存状态枚举）
- 内存节省：N 次执行共享 1 份拓扑 + N 份状态

### 4.2 方案 B：直接 Clone（简单方案）

保持现有 `Graph` 结构不变，每次执行时 `(*graph_template).clone()`。

**评估**：对于中等规模图（50-200 节点），clone 开销约为微秒级，不是瓶颈。建议初始版本使用方案 B，后续通过 benchmark 判断是否需要方案 A。

**推荐**：初始版本方案 B，后续按需升级到方案 A。理由是方案 B 实现简单且 clone 开销在中等规模图下可接受（微秒级），而方案 A 需要较大的重构来分离 Graph 结构。

## 5. 编译缓存（WorkflowCache）

为多租户 / API 服务场景提供自动缓存层。**核心约束：缓存按资源组（ResourceGroup）隔离**。

### 5.1 资源组隔离的必要性

不同资源组可能具有：
- **不同的安全策略**（SecurityPolicy）：影响可执行的节点类型、网络访问范围等
- **不同的插件配置**：同一 DSL 在不同资源组下可能加载不同的插件执行器
- **不同的凭证**（CredentialProvider）：HTTP 节点、LLM 节点等访问不同的 API endpoint
- **不同的配额**（ResourceQuota）：执行步数、超时限制不同

即使两个资源组使用完全相同的 DSL 文本，其编译产物在语义上也可能不同（例如 security gate 在编译阶段如果引入了资源组相关的验证）。因此缓存必须按资源组隔离，**不能跨资源组共享编译产物**。

### 5.2 二级缓存结构

```rust
/// 缓存键：资源组 ID + DSL 内容哈希
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct CacheKey {
    /// 资源组 ID（隔离维度）
    pub group_id: String,
    /// DSL 内容哈希
    pub content_hash: u64,
}

/// 按资源组隔离的编译缓存
///
/// 二级结构：group_id → { content_hash → CompiledWorkflow }
/// 支持按资源组独立管理缓存容量和生命周期。
pub struct WorkflowCache {
    /// 二级缓存：group_id → 该资源组的编译缓存
    groups: DashMap<String, GroupCache>,
    /// 全局配置
    config: WorkflowCacheConfig,
}

/// 单个资源组的缓存
struct GroupCache {
    entries: HashMap<u64, CacheEntry>,  // content_hash → entry
    /// 该资源组的 LRU 顺序（最近使用的 content_hash）
    lru_order: VecDeque<u64>,
}

struct CacheEntry {
    compiled: Arc<CompiledWorkflow>,
    created_at: Instant,
    last_accessed: Instant,
    access_count: u64,
}

/// 缓存配置
pub struct WorkflowCacheConfig {
    /// 每个资源组最大缓存条目数
    pub max_entries_per_group: usize,
    /// 全局最大缓存条目数（跨所有资源组总和）
    pub max_total_entries: usize,
    /// 条目 TTL
    pub ttl: Option<Duration>,
}
```

### 5.3 缓存 API

```rust
impl WorkflowCache {
    pub fn new(config: WorkflowCacheConfig) -> Self { ... }

    /// 获取或编译工作流（按资源组隔离）
    pub fn get_or_compile(
        &self,
        group_id: &str,
        content: &str,
        format: DslFormat,
    ) -> Result<Arc<CompiledWorkflow>, WorkflowError> {
        let content_hash = Self::hash_content(content);

        // 在该资源组的缓存中查找
        if let Some(mut group_cache) = self.groups.get_mut(group_id) {
            if let Some(entry) = group_cache.entries.get_mut(&content_hash) {
                if !self.is_expired(entry) {
                    entry.last_accessed = Instant::now();
                    entry.access_count += 1;
                    return Ok(entry.compiled.clone());
                }
                // 过期，移除
                group_cache.entries.remove(&content_hash);
            }
        }

        // 缓存未命中，编译
        let compiled = Arc::new(WorkflowCompiler::compile(content, format)?);

        // 存入该资源组的缓存
        let mut group_cache = self.groups
            .entry(group_id.to_string())
            .or_insert_with(GroupCache::new);
        group_cache.insert(content_hash, compiled.clone());
        self.maybe_evict_group(&mut group_cache);
        self.maybe_evict_global();

        Ok(compiled)
    }

    /// 失效指定资源组的某个工作流
    pub fn invalidate(&self, group_id: &str, content_hash: u64) {
        if let Some(mut group_cache) = self.groups.get_mut(group_id) {
            group_cache.entries.remove(&content_hash);
        }
    }

    /// 清空指定资源组的全部缓存
    pub fn clear_group(&self, group_id: &str) {
        self.groups.remove(group_id);
    }

    /// 清空全局缓存
    pub fn clear_all(&self) {
        self.groups.clear();
    }

    /// 全局缓存统计
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            group_count: self.groups.len(),
            total_entries: self.groups.iter()
                .map(|g| g.entries.len())
                .sum(),
            // ...
        }
    }

    /// 指定资源组的缓存统计
    pub fn group_stats(&self, group_id: &str) -> Option<GroupCacheStats> {
        self.groups.get(group_id).map(|g| GroupCacheStats {
            entries: g.entries.len(),
            // ...
        })
    }
}
```

### 5.4 缓存键策略

缓存使用 **二级键**：`(group_id, content_hash)`

| 维度 | 键 | 说明 |
|------|-----|------|
| 第一级 | `group_id: String` | 资源组隔离，不同资源组的缓存完全独立 |
| 第二级 | `content_hash: u64` | DSL 内容哈希，同一资源组内去重 |

**第二级键的选择**：
- 默认使用 DSL 文本内容哈希（自动去重）
- 支持用户指定 `workflow_id` 作为替代（`get_or_compile_by_id(group_id, workflow_id, content, format)`）

### 5.5 缓存淘汰

**两级淘汰**：
1. **组内淘汰**：每个资源组独立的 LRU，达到 `max_entries_per_group` 时淘汰最久未使用的条目
2. **全局淘汰**：所有资源组的总条目数达到 `max_total_entries` 时，淘汰全局最久未使用的条目
3. **TTL 过期**：条目超过 `ttl` 后在下次访问时惰性移除
4. **手动失效**：支持按 `(group_id, content_hash)` 精准失效，或按 `group_id` 批量清空

### 5.6 使用示例

```rust
let cache = WorkflowCache::new(WorkflowCacheConfig {
    max_entries_per_group: 50,
    max_total_entries: 500,
    ttl: Some(Duration::from_secs(3600)),
});

// 资源组 A 的工作流
let compiled_a = cache.get_or_compile("group_a", yaml, DslFormat::Yaml)?;
let handle = compiled_a.runner()
    .user_inputs(inputs)
    .run()
    .await?;

// 资源组 B 使用相同 DSL，但有独立的编译缓存
let compiled_b = cache.get_or_compile("group_b", yaml, DslFormat::Yaml)?;

// 资源组 A 的工作流更新了 DSL，主动失效旧缓存
cache.clear_group("group_a");

// 资源组被删除时，清理其缓存
cache.clear_group("deleted_group");
```

## 6. 与现有 API 的兼容

### 6.1 保留现有接口

现有 `WorkflowRunner::builder(schema).run()` 接口 **完全保留不变**，作为"解释执行"模式：

```rust
// 旧接口，保持不变（每次完整流水线）
let handle = WorkflowRunner::builder(schema)
    .user_inputs(inputs)
    .run()
    .await?;
```

### 6.2 新增编译接口

```rust
// 方式 1：显式编译
let compiled = WorkflowCompiler::compile(yaml, DslFormat::Yaml)?;
let handle = compiled.runner().user_inputs(inputs).run().await?;

// 方式 2：从 schema 编译
let compiled = WorkflowCompiler::compile_schema(schema)?;

// 方式 3：带缓存的编译（按资源组隔离）
let cache = WorkflowCache::new(WorkflowCacheConfig {
    max_entries_per_group: 50,
    max_total_entries: 500,
    ttl: Some(Duration::from_secs(3600)),
});
let compiled = cache.get_or_compile("group_a", yaml, DslFormat::Yaml)?;
let handle = compiled.runner().user_inputs(inputs).run().await?;
```

### 6.3 WorkflowRunner 便捷方法

在现有 `WorkflowRunner` 上添加 `compile` 便捷方法：

```rust
impl WorkflowRunner {
    /// 编译工作流（新增）
    pub fn compile(
        content: &str,
        format: DslFormat,
    ) -> Result<CompiledWorkflow, WorkflowError> {
        WorkflowCompiler::compile(content, format)
    }

    /// 从 schema 编译（新增）
    pub fn compile_schema(
        schema: WorkflowSchema,
    ) -> Result<CompiledWorkflow, WorkflowError> {
        WorkflowCompiler::compile_schema(schema)
    }
}
```

## 7. 模块结构

```
src/
├── compiler/
│   ├── mod.rs                  # re-exports
│   ├── compiled_workflow.rs    # CompiledWorkflow 结构
│   ├── compiler.rs             # WorkflowCompiler 编译逻辑
│   ├── runner.rs               # CompiledWorkflowRunnerBuilder
│   └── cache.rs                # WorkflowCache (feature-gated)
├── scheduler.rs                # 现有 WorkflowRunner（不变）
├── dsl/                        # 现有解析/验证（不变）
└── graph/                      # 现有图结构（不变）
```

### Feature Gate

```toml
[features]
default = ["compiler"]
compiler = []
workflow-cache = ["compiler", "dep:dashmap"]
```

## 8. 执行流程对比

### 8.1 当前流程（解释执行）

```
每次 run():
  parse_dsl()           ~100-500 μs  (取决于 DSL 大小)
  validate_schema()     ~50-200 μs   (三层验证)
  build_graph()         ~20-80 μs    (HashMap 分配)
  collect_types()       ~5-10 μs
  init_pool()           ~10-20 μs
  build_registry()      ~5-10 μs
  dispatcher.run()      ~执行时间
  ────────────────────────────────
  总编译开销: ~180-820 μs / 次
```

### 8.2 编译模式

```
compile() (一次性):
  parse_dsl()           ~100-500 μs
  validate_schema()     ~50-200 μs
  build_graph()         ~20-80 μs
  collect_types()       ~5-10 μs
  compile_configs()     ~10-30 μs
  ────────────────────────────────
  总编译开销: ~180-820 μs (仅一次)

每次 run():
  security_validate()   ~5-10 μs   (轻量权限检查)
  clone_graph()         ~10-40 μs  (或方案 A 接近零)
  init_pool()           ~10-20 μs
  build_registry()      ~5-10 μs
  dispatcher.run()      ~执行时间
  ────────────────────────────────
  总 per-run 开销: ~30-80 μs / 次
```

**节省**：每次执行节省约 **150-740 μs**。在高频调用场景（如 API 服务每秒数百次），累计节省显著。

## 9. 线程安全与并发

### 9.1 CompiledWorkflow 并发共享

```rust
// CompiledWorkflow 内部全部 Arc，安全共享
let compiled = Arc::new(WorkflowCompiler::compile(yaml, DslFormat::Yaml)?);

// 并发启动多个执行实例
let compiled_clone = compiled.clone();
tokio::spawn(async move {
    compiled_clone.runner().user_inputs(inputs1).run().await
});

let compiled_clone2 = compiled.clone();
tokio::spawn(async move {
    compiled_clone2.runner().user_inputs(inputs2).run().await
});
```

### 9.2 不可变性保证

`CompiledWorkflow` 的所有字段都是不可变的（Arc 包裹）。运行时可变状态（Graph 边状态、VariablePool）在每次 `run()` 时独立创建，不影响编译产物。

## 10. 错误处理

### 10.1 编译时错误

编译时就能发现的错误在 `compile()` 阶段报告，运行时不再重复检查：

```rust
match WorkflowCompiler::compile(yaml, DslFormat::Yaml) {
    Ok(compiled) => {
        // 可检查 warnings
        for diag in compiled.validation_report().diagnostics.iter() {
            if diag.level == DiagnosticLevel::Warning {
                log::warn!("{}", diag.message);
            }
        }
    }
    Err(WorkflowError::ParseError(e)) => { /* DSL 语法错误 */ }
    Err(WorkflowError::ValidationFailed(report)) => { /* 验证错误 */ }
    Err(e) => { /* 其他错误 */ }
}
```

### 10.2 运行时错误

运行时错误（节点执行失败、超时等）仍通过 `ExecutionStatus::Failed` 报告，与现有行为一致。

## 11. 后续扩展

### 11.1 模板预编译

当前 Jinja2 模板在每次节点执行时通过 `env.add_template() → env.get_template()` 编译渲染。可以在编译阶段预编译：

```rust
// 编译阶段：预编译所有模板
pub struct CompiledTemplates {
    templates: HashMap<String, Box<dyn CompiledTemplateHandle>>,
    // node_id → compiled template
}
```

**限制**：`minijinja::Environment` 不是 Send + Sync（或其编译结果不易跨线程共享），需要评估具体实现。可作为后续优化点。

### 11.2 序列化编译产物

未来可将 `CompiledWorkflow` 序列化到磁盘/Redis，支持跨进程复用：

```rust
impl CompiledWorkflow {
    pub fn serialize(&self) -> Vec<u8> { ... }
    pub fn deserialize(data: &[u8]) -> Result<Self, Error> { ... }
}
```

**前提**：所有内部类型实现 `Serialize` / `Deserialize`。当前 `Graph` 已经 derive 了 `Clone`/`Debug`，添加 Serde 支持工作量不大。

### ~~11.3 增量编译~~

~~当 DSL 发生局部变更时，只重新编译变更的节点/边，复用未变更部分。适用于工作流编辑器场景。~~

### 11.4 与 RuntimeGroup 集成

参考 `runtime-context-layered-architecture-design.md`，`CompiledWorkflow` 天然适合放入 `RuntimeGroup`（共享配置层），供同一工作流的多个实例复用。而 `WorkflowCache` 按资源组隔离的设计与 `RuntimeGroup` 的分组模型天然对齐：

```rust
pub struct RuntimeGroup {
    group_id: String,
    /// 该资源组的编译缓存（从全局 WorkflowCache 中按 group_id 隔离）
    workflow_cache: Arc<WorkflowCache>,
    shared_sandbox_pool: Arc<SandboxPool>,
    shared_llm_registry: Arc<LlmProviderRegistry>,
    // ...
}

impl RuntimeGroup {
    /// 获取或编译工作流（自动使用本资源组的缓存分区）
    pub fn get_or_compile(
        &self,
        content: &str,
        format: DslFormat,
    ) -> Result<Arc<CompiledWorkflow>, WorkflowError> {
        self.workflow_cache.get_or_compile(&self.group_id, content, format)
    }
}

impl Drop for RuntimeGroup {
    fn drop(&mut self) {
        // 资源组销毁时清理其缓存分区
        self.workflow_cache.clear_group(&self.group_id);
    }
}
```

## 12. 实现优先级

| 阶段 | 内容 | 优先级 |
|------|------|--------|
| P0 | `CompiledWorkflow` 核心结构 + `WorkflowCompiler::compile()` | 必须 |
| P0 | `CompiledWorkflowRunnerBuilder` + `run()` | 必须 |
| P0 | 保持现有 `WorkflowRunner` 接口不变 | 必须 |
| P0 | `WorkflowCache` 按资源组隔离的编译缓存 | 必须 |
| P0 | Graph Clone 优化（方案 A：拓扑/状态分离） | 必须 |
| P0 | `CompiledNodeConfig` 节点配置预编译 | 必须 |
| P2 | 模板预编译 | 可选 |
| P3 | 编译产物序列化 | 远期 |
| P3 | 增量编译 | 远期 |

## 13. 总结

编译功能通过将 DSL 解析、验证、图构建等确定性步骤从每次执行中提取到一次性编译阶段，实现了：

1. **性能提升**：消除重复解析/验证开销，每次执行节省 ~150-740μs
2. **架构清晰**：明确区分"编译时"与"运行时"，职责分离
3. **API 优雅**：`compiled.runner().user_inputs(x).run()` 语义清晰
4. **向后兼容**：现有 `WorkflowRunner` 接口完全保留
5. **并发友好**：`CompiledWorkflow` 天然 Send+Sync，可安全共享
6. **可扩展**：为缓存、序列化、增量编译预留了空间
