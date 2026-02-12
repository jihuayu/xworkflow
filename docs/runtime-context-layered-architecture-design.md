# RuntimeContext 分层架构设计

## 一、问题分析

### 1.1 当前架构的问题

**现状**：RuntimeContext 混合了两种不同生命周期的数据：

```rust
pub struct RuntimeContext {
    // 共享配置（应该在多个工作流间共享）
    pub time_provider: Arc<dyn TimeProvider>,
    pub id_generator: Arc<dyn IdGenerator>,
    pub extensions: RuntimeExtensions {
        pub node_executor_registry: Option<Arc<NodeExecutorRegistry>>,  // 插件注册表
        pub template_functions: Option<Arc<HashMap<...>>>,              // 模板函数
        pub security: Option<SecurityContext> {
            pub quota: ResourceQuota,                    // 资源限额
            pub credential_provider: ...,                // 凭证提供者
            pub resource_governor: ...,                  // 配额管理器
        },
    },

    // 工作流实例特定（每个工作流应该独有）
    pub extensions: RuntimeExtensions {
        pub event_tx: Option<mpsc::Sender<...>>,        // 事件通道（每个工作流独立）
        pub sub_graph_runner: Option<Arc<...>>,         // 子图运行器（每个工作流独立）
    },
}
```

**问题**：
1. **资源浪费**：每个工作流都创建自己的 NodeExecutorRegistry、虚拟机池等
2. **配置不一致**：同一个"运行组"的工作流可能使用不同的配置
3. **无法共享资源**：LLM 连接池、虚拟机池无法在多个工作流间共享
4. **概念混乱**：共享配置和实例状态混在一起

### 1.2 用户需求

**共享部分**（运行组级别）：
- LLM 连接参数和连接池
- 执行资源限额（配额）
- 注册的插件（NodeExecutorRegistry）
- 共享的虚拟机池（JS/WASM sandbox pool）
- 凭证提供者
- 审计日志器

**独有部分**（工作流实例级别）：
- 执行 ID（execution_id）
- 执行开始时间（start_time）
- 事件通道（event_tx）
- 子图运行器（sub_graph_runner）
- 工作流特定的变量池

---

## 二、新架构设计

### 2.1 架构分层

```
┌─────────────────────────────────────────────────────────────┐
│ RuntimeGroup (运行组 - 共享配置)                             │
│ - 生命周期：长期存在，跨多个工作流                            │
│ - 共享方式：Arc<RuntimeGroup>                                │
├─────────────────────────────────────────────────────────────┤
│ - LLM 连接池                                                 │
│ - 虚拟机池（JS/WASM sandbox pool）                           │
│ - 插件注册表（NodeExecutorRegistry）                         │
│ - 资源限额（ResourceQuota）                                  │
│ - 凭证提供者（CredentialProvider）                           │
│ - 审计日志器（AuditLogger）                                  │
│ - 资源治理器（ResourceGovernor）                             │
└─────────────────────────────────────────────────────────────┘
                            ↓ Arc 引用
┌─────────────────────────────────────────────────────────────┐
│ WorkflowContext (工作流上下文 - 实例特定)                    │
│ - 生命周期：单个工作流执行期间                                │
│ - 共享方式：Arc<WorkflowContext>                             │
├─────────────────────────────────────────────────────────────┤
│ - runtime_group: Arc<RuntimeGroup>  ← 引用共享配置           │
│ - execution_id: String                                       │
│ - workflow_id: Option<String>                                │
│ - start_time: i64                                            │
│ - event_tx: Option<mpsc::Sender<...>>                        │
│ - sub_graph_runner: Option<Arc<dyn SubGraphRunner>>          │
│ - time_provider: Arc<dyn TimeProvider>                       │
│ - id_generator: Arc<dyn IdGenerator>                         │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 结构定义

#### RuntimeGroup（运行组）

```rust
/// 运行组：多个工作流共享的配置和资源池
///
/// 生命周期：长期存在，通常在应用启动时创建，关闭时销毁
/// 使用场景：
/// - 同一个租户/用户的所有工作流共享一个 RuntimeGroup
/// - 同一个服务实例的所有工作流共享一个 RuntimeGroup
#[derive(Clone)]
pub struct RuntimeGroup {
    /// 运行组 ID（可选，用于标识）
    pub group_id: Option<String>,

    /// 运行组名称（可选，用于显示）
    pub group_name: Option<String>,

    // ===== 共享资源池 =====

    /// 节点执行器注册表（插件系统）
    pub node_executor_registry: Arc<NodeExecutorRegistry>,

    /// LLM 提供者注册表（连接池）
    pub llm_provider_registry: Arc<LlmProviderRegistry>,

    /// 模板函数注册表（插件提供的自定义函数）
    #[cfg(feature = "plugin-system")]
    pub template_functions: Arc<HashMap<String, Arc<dyn TemplateFunction>>>,

    /// 虚拟机池（JS/WASM sandbox pool）
    /// 多个工作流共享同一个虚拟机池，提高资源利用率
    #[cfg(feature = "sandbox")]
    pub sandbox_pool: Arc<dyn SandboxPool>,

    // ===== 安全配置 =====

    /// 资源配额（限制工作流的资源使用）
    pub quota: ResourceQuota,

    /// 安全级别
    pub security_level: SecurityLevel,

    /// 安全策略
    pub security_policy: Option<SecurityPolicy>,

    /// 凭证提供者（API Key 等）
    pub credential_provider: Option<Arc<dyn CredentialProvider>>,

    /// 资源治理器（配额管理）
    pub resource_governor: Option<Arc<dyn ResourceGovernor>>,

    /// 审计日志器
    pub audit_logger: Option<Arc<dyn AuditLogger>>,
}

impl RuntimeGroup {
    /// 创建默认的运行组
    pub fn default() -> Self {
        Self {
            group_id: None,
            group_name: None,
            node_executor_registry: Arc::new(NodeExecutorRegistry::new()),
            llm_provider_registry: Arc::new(LlmProviderRegistry::new()),
            #[cfg(feature = "plugin-system")]
            template_functions: Arc::new(HashMap::new()),
            #[cfg(feature = "sandbox")]
            sandbox_pool: Arc::new(DefaultSandboxPool::new()),
            quota: ResourceQuota::default(),
            security_level: SecurityLevel::Standard,
            security_policy: None,
            credential_provider: None,
            resource_governor: None,
            audit_logger: None,
        }
    }

    /// 创建 Builder
    pub fn builder() -> RuntimeGroupBuilder {
        RuntimeGroupBuilder::new()
    }
}
```

#### WorkflowContext（工作流上下文）

```rust
/// 工作流上下文：单个工作流实例的执行上下文
///
/// 生命周期：单个工作流执行期间
/// 使用场景：每个工作流实例创建一个 WorkflowContext
#[derive(Clone)]
pub struct WorkflowContext {
    // ===== 引用共享配置 =====

    /// 运行组（共享配置和资源池）
    pub runtime_group: Arc<RuntimeGroup>,

    // ===== 工作流实例特定 =====

    /// 执行 ID（唯一标识本次执行）
    pub execution_id: String,

    /// 工作流 ID（可选，用于审计）
    pub workflow_id: Option<String>,

    /// 执行开始时间（Unix 时间戳）
    pub start_time: i64,

    /// 时间提供者（支持测试注入 Fake）
    pub time_provider: Arc<dyn TimeProvider>,

    /// ID 生成器（支持测试注入 Fake）
    pub id_generator: Arc<dyn IdGenerator>,

    /// 事件通道（发送工作流事件）
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,

    /// 子图运行器（支持嵌套工作流）
    pub sub_graph_runner: Option<Arc<dyn SubGraphRunner>>,

    /// 是否启用严格模板模式
    pub strict_template: bool,
}

impl WorkflowContext {
    /// 创建新的工作流上下文
    pub fn new(runtime_group: Arc<RuntimeGroup>) -> Self {
        let time_provider = Arc::new(RealTimeProvider::default());
        let id_generator = Arc::new(RealIdGenerator::default());
        let execution_id = id_generator.next_id();
        let start_time = time_provider.now_timestamp();

        Self {
            runtime_group,
            execution_id,
            workflow_id: None,
            start_time,
            time_provider,
            id_generator,
            event_tx: None,
            sub_graph_runner: None,
            strict_template: false,
        }
    }

    /// 便捷方法：访问运行组的配额
    pub fn quota(&self) -> &ResourceQuota {
        &self.runtime_group.quota
    }

    /// 便捷方法：访问运行组的凭证提供者
    pub fn credential_provider(&self) -> Option<&Arc<dyn CredentialProvider>> {
        self.runtime_group.credential_provider.as_ref()
    }

    /// 便捷方法：访问运行组的节点执行器注册表
    pub fn node_executor_registry(&self) -> &Arc<NodeExecutorRegistry> {
        &self.runtime_group.node_executor_registry
    }

    /// 便捷方法：访问运行组的 LLM 提供者注册表
    pub fn llm_provider_registry(&self) -> &Arc<LlmProviderRegistry> {
        &self.runtime_group.llm_provider_registry
    }

    /// 便捷方法：访问运行组的虚拟机池
    #[cfg(feature = "sandbox")]
    pub fn sandbox_pool(&self) -> &Arc<dyn SandboxPool> {
        &self.runtime_group.sandbox_pool
    }
}
```

---

## 三、使用示例

### 3.1 创建运行组（应用启动时）

```rust
// 应用启动时创建运行组
let runtime_group = RuntimeGroup::builder()
    .group_id("tenant-123")
    .group_name("Tenant 123")
    .quota(ResourceQuota {
        max_concurrent_workflows: 10,
        max_steps: 500,
        max_execution_time_secs: 600,
        ..Default::default()
    })
    .credential_provider(Arc::new(MyCredentialProvider::new()))
    .audit_logger(Arc::new(MyAuditLogger::new()))
    .build();

let runtime_group = Arc::new(runtime_group);

// 运行组长期存在，可以被多个工作流共享
```

### 3.2 执行工作流（每次执行创建新的 WorkflowContext）

```rust
// 工作流 1
let workflow_ctx_1 = WorkflowContext::new(Arc::clone(&runtime_group));
let runner_1 = WorkflowRunner::builder(schema_1)
    .context(workflow_ctx_1)
    .user_inputs(inputs_1)
    .run().await?;

// 工作流 2（同时执行，共享同一个运行组）
let workflow_ctx_2 = WorkflowContext::new(Arc::clone(&runtime_group));
let runner_2 = WorkflowRunner::builder(schema_2)
    .context(workflow_ctx_2)
    .user_inputs(inputs_2)
    .run().await?;

// 两个工作流共享：
// - LLM 连接池
// - 虚拟机池
// - 插件注册表
// - 配额限制
//
// 但各自有独立的：
// - execution_id
// - start_time
// - event_tx
```

### 3.3 节点执行器访问

```rust
impl NodeExecutor for MyNodeExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &WorkflowContext,  // ← 接收 WorkflowContext
    ) -> Result<NodeRunResult, NodeError> {
        // 访问共享资源
        let llm_registry = context.llm_provider_registry();
        let sandbox_pool = context.sandbox_pool();
        let quota = context.quota();

        // 访问工作流特定信息
        let execution_id = &context.execution_id;
        let start_time = context.start_time;

        // ...
    }
}
```

---

## 四、迁移计划

### 4.1 阶段 1：创建新结构

1. **创建 `RuntimeGroup` 结构体**
   - 文件：`src/core/runtime_group.rs`
   - 包含所有共享配置和资源池

2. **重命名 `RuntimeContext` 为 `WorkflowContext`**
   - 文件：`src/core/workflow_context.rs`
   - 包含工作流实例特定的信息
   - 持有 `Arc<RuntimeGroup>` 引用

3. **创建 Builder**
   - `RuntimeGroupBuilder`
   - `WorkflowContextBuilder`（可选，因为 WorkflowContext 可以直接 new）

### 4.2 阶段 2：更新调用方

1. **WorkflowRunner**
   ```rust
   pub struct WorkflowRunner {
       context: WorkflowContext,  // 原 RuntimeContext
       // ...
   }

   impl WorkflowRunner {
       pub fn builder(schema: WorkflowSchema) -> WorkflowRunnerBuilder {
           // 需要传入 RuntimeGroup 或使用默认的
           WorkflowRunnerBuilder::new(schema)
       }
   }
   ```

2. **WorkflowDispatcher**
   ```rust
   pub struct WorkflowDispatcher {
       context: Arc<WorkflowContext>,  // 原 Arc<RuntimeContext>
       // ...
   }
   ```

3. **NodeExecutor trait**
   ```rust
   pub trait NodeExecutor {
       async fn execute(
           &self,
           node_id: &str,
           config: &Value,
           variable_pool: &VariablePool,
           context: &WorkflowContext,  // 原 &RuntimeContext
       ) -> Result<NodeRunResult, NodeError>;
   }
   ```

### 4.3 阶段 3：虚拟机池实现

```rust
/// 虚拟机池 trait
pub trait SandboxPool: Send + Sync {
    /// 获取一个可用的 JS 虚拟机
    async fn acquire_js_vm(&self) -> Result<Box<dyn JsRuntime>, SandboxError>;

    /// 归还 JS 虚拟机
    async fn release_js_vm(&self, vm: Box<dyn JsRuntime>);

    /// 获取一个可用的 WASM 虚拟机
    async fn acquire_wasm_vm(&self) -> Result<Box<dyn WasmRuntime>, SandboxError>;

    /// 归还 WASM 虚拟机
    async fn release_wasm_vm(&self, vm: Box<dyn WasmRuntime>);
}

/// 默认实现：对象池模式
pub struct DefaultSandboxPool {
    js_pool: Arc<Mutex<Vec<Box<dyn JsRuntime>>>>,
    wasm_pool: Arc<Mutex<Vec<Box<dyn WasmRuntime>>>>,
    max_js_vms: usize,
    max_wasm_vms: usize,
}
```

---

## 五、优势分析

### 5.1 资源共享

**之前**：每个工作流创建自己的资源
```rust
// 工作流 1
let registry_1 = Arc::new(NodeExecutorRegistry::new());  // 创建新的
let llm_registry_1 = Arc::new(LlmProviderRegistry::new());  // 创建新的

// 工作流 2
let registry_2 = Arc::new(NodeExecutorRegistry::new());  // 又创建新的
let llm_registry_2 = Arc::new(LlmProviderRegistry::new());  // 又创建新的
```

**现在**：多个工作流共享同一个资源
```rust
// 应用启动时创建一次
let runtime_group = Arc::new(RuntimeGroup::builder()
    .node_executor_registry(registry)
    .llm_provider_registry(llm_registry)
    .build());

// 工作流 1 和 2 共享
let ctx_1 = WorkflowContext::new(Arc::clone(&runtime_group));
let ctx_2 = WorkflowContext::new(Arc::clone(&runtime_group));
```

### 5.2 配置统一

**之前**：每个工作流可能有不同的配额
```rust
let runner_1 = WorkflowRunner::builder(schema_1)
    .quota(ResourceQuota { max_steps: 100, ... })
    .run().await?;

let runner_2 = WorkflowRunner::builder(schema_2)
    .quota(ResourceQuota { max_steps: 200, ... })  // 不一致！
    .run().await?;
```

**现在**：同一个运行组的工作流使用相同的配额
```rust
let runtime_group = Arc::new(RuntimeGroup::builder()
    .quota(ResourceQuota { max_steps: 100, ... })
    .build());

// 所有工作流自动使用相同的配额
let ctx_1 = WorkflowContext::new(Arc::clone(&runtime_group));
let ctx_2 = WorkflowContext::new(Arc::clone(&runtime_group));
```

### 5.3 性能优化

| 资源 | 之前 | 现在 | 优化效果 |
|------|------|------|----------|
| **虚拟机池** | 每个工作流创建新的 VM | 共享 VM 池 | 减少 VM 创建开销 |
| **LLM 连接** | 每个工作流创建新连接 | 共享连接池 | 减少连接建立开销 |
| **插件注册表** | 每个工作流复制一份 | 共享注册表 | 减少内存占用 |
| **配额管理器** | 每个工作流独立计数 | 统一计数 | 准确的配额控制 |

### 5.4 隔离性

| 信息 | 共享 | 隔离 |
|------|------|------|
| **execution_id** | ❌ | ✅ 每个工作流独立 |
| **start_time** | ❌ | ✅ 每个工作流独立 |
| **event_tx** | ❌ | ✅ 每个工作流独立 |
| **LLM 连接池** | ✅ 共享 | ❌ |
| **虚拟机池** | ✅ 共享 | ❌ |
| **配额** | ✅ 共享 | ❌ |

---

## 六、向后兼容性

### 6.1 类型别名（过渡期）

```rust
// 提供类型别名，减少迁移成本
#[deprecated(note = "Use WorkflowContext instead")]
pub type RuntimeContext = WorkflowContext;
```

### 6.2 默认行为

```rust
impl WorkflowRunner {
    pub fn builder(schema: WorkflowSchema) -> WorkflowRunnerBuilder {
        // 如果用户没有提供 RuntimeGroup，使用默认的
        WorkflowRunnerBuilder {
            schema,
            runtime_group: Arc::new(RuntimeGroup::default()),
            // ...
        }
    }
}
```

---

## 七、总结

### 7.1 核心改进

1. **概念清晰**：RuntimeGroup（共享配置）vs WorkflowContext（实例状态）
2. **资源共享**：LLM 连接池、虚拟机池在多个工作流间共享
3. **配置统一**：同一个运行组的工作流使用相同的配额和策略
4. **性能优化**：减少资源创建开销，提高资源利用率
5. **隔离性**：每个工作流有独立的执行 ID 和事件通道

### 7.2 架构对比

| 维度 | 旧架构 | 新架构 |
|------|--------|--------|
| **结构** | RuntimeContext（混合） | RuntimeGroup + WorkflowContext（分层） |
| **资源共享** | 每个工作流独立 | 多个工作流共享 |
| **配置管理** | 分散在各个工作流 | 集中在 RuntimeGroup |
| **生命周期** | 与工作流绑定 | RuntimeGroup 长期存在 |
| **扩展性** | 难以扩展 | 易于扩展（如添加新的共享资源） |

### 7.3 实施建议

1. **优先级 P0**：创建 RuntimeGroup 和 WorkflowContext 结构
2. **优先级 P1**：实现虚拟机池（SandboxPool）
3. **优先级 P2**：迁移现有代码
4. **优先级 P3**：性能测试和优化
