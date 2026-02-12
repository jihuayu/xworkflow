# RuntimeContext 架构分析

## 一、架构地位

### 1.1 定位

**RuntimeContext 是工作流执行的全局上下文容器**，贯穿整个工作流生命周期，为所有组件提供：
- 时间服务（TimeProvider）
- ID 生成服务（IdGenerator）
- 扩展服务（事件总线、子图运行器、插件、安全配置等）

### 1.2 在架构中的位置

```
用户代码
  ↓
WorkflowRunner (持有 RuntimeContext 值)
  ↓ 构建阶段：配置 RuntimeContext
  ↓ 执行阶段：Arc::new(context)
  ↓
WorkflowDispatcher (持有 Arc<RuntimeContext>)
  ↓ 执行节点时：&Arc<RuntimeContext> → &RuntimeContext
  ↓
NodeExecutor (接收 &RuntimeContext)
  ↓ 读取配置、调用服务
  ↓
具体节点实现（LLM、HTTP、Code 等）
```

### 1.3 核心职责

| 职责 | 说明 | 示例 |
|------|------|------|
| **服务定位器** | 提供全局服务的访问入口 | `context.time_provider.now_timestamp()` |
| **配置容器** | 携带执行配置（安全策略、配额、模板函数等） | `context.security()?.quota` |
| **依赖注入** | 将外部依赖注入到节点执行器 | `context.credential_provider()` |
| **上下文传递** | 在调用链中传递上下文信息 | 从 Dispatcher → Executor → 具体实现 |

---

## 二、生命周期

### 2.1 完整生命周期图

```
┌─────────────────────────────────────────────────────────────┐
│ 阶段 1：构建阶段（Builder Pattern）                          │
├─────────────────────────────────────────────────────────────┤
│ WorkflowRunner::builder(schema)                             │
│   ↓                                                          │
│ WorkflowRunnerBuilder {                                     │
│   context: RuntimeContext::default()  ← 创建默认 context    │
│ }                                                            │
│   ↓                                                          │
│ .security_policy(...)     ← 配置安全策略                     │
│ .resource_group(...)      ← 配置资源组/租户                  │
│ .llm_providers(...)       ← 配置 LLM 提供者                  │
│ .user_inputs(...)         ← 设置用户输入                     │
│   ↓                                                          │
│ 插件系统修改 context：                                       │
│   - plugin_gate.customize_context(&mut context)             │
│   - 注入 template_functions                                 │
│   - 注入 time_provider / id_generator                       │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ 阶段 2：冻结阶段（Arc 包装）                                 │
├─────────────────────────────────────────────────────────────┤
│ .run() 或 .run_debug()                                      │
│   ↓                                                          │
│ let context = Arc::new(                                     │
│   builder.context.with_event_tx(tx)  ← 添加事件通道         │
│ );                                                           │
│   ↓                                                          │
│ RuntimeContext 被包装成 Arc，从此不可变                      │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ 阶段 3：执行阶段（共享只读访问）                             │
├─────────────────────────────────────────────────────────────┤
│ WorkflowDispatcher::new(                                    │
│   context.clone()  ← Arc::clone()，增加引用计数             │
│ )                                                            │
│   ↓                                                          │
│ dispatcher.run().await                                      │
│   ↓                                                          │
│ 执行每个节点：                                               │
│   executor.execute(                                         │
│     node_id,                                                │
│     config,                                                 │
│     pool_snapshot,                                          │
│     &self.context  ← &Arc<RuntimeContext> → &RuntimeContext │
│   )                                                          │
│   ↓                                                          │
│ 节点内部读取 context：                                       │
│   - context.time_provider.now_timestamp()                   │
│   - context.security()?.quota                               │
│   - context.credential_provider()                           │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ 阶段 4：销毁阶段（引用计数归零）                             │
├─────────────────────────────────────────────────────────────┤
│ dispatcher 执行完成或失败                                    │
│   ↓                                                          │
│ tokio::spawn 的闭包结束                                      │
│   ↓                                                          │
│ Arc<RuntimeContext> 引用计数递减                             │
│   ↓                                                          │
│ 当所有引用都释放后，RuntimeContext 被销毁                    │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 关键时间点

| 时间点 | 操作 | 可变性 |
|--------|------|--------|
| **T0: 创建** | `RuntimeContext::default()` | ✅ 可变 |
| **T1: 配置** | Builder 方法修改 context | ✅ 可变 |
| **T2: 插件定制** | `plugin_gate.customize_context(&mut context)` | ✅ 可变 |
| **T3: 冻结** | `Arc::new(context)` | ❌ 不可变 |
| **T4: 执行** | 节点读取 context | ❌ 只读 |
| **T5: 销毁** | Arc 引用计数归零 | - |

---

## 三、核心作用

### 3.1 时间与 ID 服务

```rust
pub struct RuntimeContext {
    pub time_provider: Arc<dyn TimeProvider>,  // 时间服务
    pub id_generator: Arc<dyn IdGenerator>,    // ID 生成服务
    // ...
}
```

**用途**：
- **时间服务**：为节点提供统一的时间戳（支持测试时注入 FakeTimeProvider）
- **ID 生成**：为执行 ID、节点 ID 等生成唯一标识（支持测试时注入 FakeIdGenerator）

**示例**：
```rust
// 节点执行时获取时间戳
let timestamp = context.time_provider.now_timestamp();

// 生成唯一 ID
let exec_id = context.id_generator.next_id();
```

### 3.2 事件总线

```rust
pub struct RuntimeExtensions {
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    // ...
}
```

**用途**：
- 节点执行时发送事件（NodeRunStarted, NodeRunCompleted 等）
- 用户通过 `WorkflowHandle.events()` 获取事件流

**示例**：
```rust
if let Some(tx) = context.event_tx() {
    tx.send(GraphEngineEvent::NodeRunStarted { ... }).await;
}
```

### 3.3 子图运行器

```rust
pub struct RuntimeExtensions {
    pub sub_graph_runner: Option<Arc<dyn SubGraphRunner>>,
    // ...
}
```

**用途**：
- 支持嵌套工作流（子图节点）
- 递归执行子工作流

### 3.4 节点执行器注册表

```rust
pub struct RuntimeExtensions {
    pub node_executor_registry: Option<Arc<NodeExecutorRegistry>>,
    // ...
}
```

**用途**：
- 动态查找节点类型对应的执行器
- 支持插件注册自定义节点

### 3.5 模板函数

```rust
#[cfg(feature = "plugin-system")]
pub struct RuntimeExtensions {
    pub template_functions: Option<Arc<HashMap<String, Arc<dyn TemplateFunction>>>>,
    // ...
}
```

**用途**：
- 插件注册自定义模板函数（如 `{{ my_plugin::custom_func() }}`）
- 在模板渲染时调用

### 3.6 安全上下文

```rust
#[cfg(feature = "security")]
pub struct RuntimeExtensions {
    pub security: Option<SecurityContext>,
}

pub struct SecurityContext {
    pub resource_group: Option<ResourceGroup>,      // 租户/资源组
    pub security_policy: Option<SecurityPolicy>,    // 安全策略
    pub resource_governor: Option<Arc<dyn ResourceGovernor>>,  // 配额管理
    pub credential_provider: Option<Arc<dyn CredentialProvider>>,  // 凭证提供者
    pub audit_logger: Option<Arc<dyn AuditLogger>>,  // 审计日志
}
```

**用途**：
- **配额管理**：限制工作流资源使用（最大步数、执行时间、HTTP 请求速率等）
- **安全策略**：控制节点的安全级别（Strict/Standard/Permissive）
- **凭证管理**：为 LLM、HTTP 等节点提供 API Key
- **审计日志**：记录工作流执行的安全事件

**示例**：
```rust
// 获取配额
if let Some(quota) = context.security().and_then(|s| s.resource_group.as_ref()) {
    let max_steps = quota.quota.max_steps;
}

// 获取凭证
if let Some(provider) = context.credential_provider() {
    let api_key = provider.get_credential("openai_api_key").await?;
}
```

---

## 四、设计模式分析

### 4.1 服务定位器模式（Service Locator）

RuntimeContext 充当服务定位器，提供全局服务的访问入口：

```rust
// 不需要在每个节点中传递 TimeProvider、IdGenerator 等
// 统一通过 context 访问
let timestamp = context.time_provider.now_timestamp();
let credential = context.credential_provider()?.get_credential("key").await?;
```

### 4.2 依赖注入模式（Dependency Injection）

通过 Builder 模式注入依赖：

```rust
WorkflowRunner::builder(schema)
    .security_policy(policy)           // 注入安全策略
    .resource_group(group)             // 注入资源组
    .llm_providers(registry)           // 注入 LLM 提供者
    .run().await
```

### 4.3 不可变共享模式（Immutable Sharing）

- **构建阶段**：可变的 `RuntimeContext`
- **执行阶段**：不可变的 `Arc<RuntimeContext>`
- **线程安全**：通过 Arc + 不可变引用保证

### 4.4 扩展点模式（Extension Point）

`RuntimeExtensions` 提供扩展点，支持：
- 插件系统注入自定义服务
- 安全模块注入安全配置
- 调试模块注入调试钩子

---

## 五、与其他组件的关系

### 5.1 与 VariablePool 的关系

| 组件 | 职责 | 生命周期 |
|------|------|----------|
| **RuntimeContext** | 提供全局服务和配置 | 整个工作流执行期间 |
| **VariablePool** | 存储节点间传递的变量 | 整个工作流执行期间 |

**区别**：
- RuntimeContext 是**只读的上下文**，不会在执行期间修改
- VariablePool 是**可变的状态**，节点执行会修改变量池

### 5.2 与 Dispatcher 的关系

```rust
pub struct WorkflowDispatcher {
    context: Arc<RuntimeContext>,      // 持有 context
    variable_pool: Arc<RwLock<VariablePool>>,  // 持有 pool
    // ...
}
```

- **Dispatcher 是执行引擎**：驱动 DAG 图的执行
- **RuntimeContext 是执行上下文**：为 Dispatcher 提供服务和配置

### 5.3 与 NodeExecutor 的关系

```rust
pub trait NodeExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,  // ← 接收 context
    ) -> Result<NodeRunResult, NodeError>;
}
```

- **NodeExecutor 是节点实现**：执行具体的业务逻辑
- **RuntimeContext 是依赖来源**：提供节点所需的服务（时间、凭证、事件等）

---

## 六、线程安全性保证

### 6.1 共享机制

```rust
// Dispatcher 持有 Arc<RuntimeContext>
pub struct WorkflowDispatcher {
    context: Arc<RuntimeContext>,  // ← 共享所有权
}

// 节点执行时传递引用
executor.execute(node_id, config, pool, &self.context)
// ← &Arc<RuntimeContext> 自动解引用为 &RuntimeContext
```

### 6.2 线程安全性分析

| 字段 | 类型 | 线程安全机制 |
|------|------|-------------|
| `time_provider` | `Arc<dyn TimeProvider>` | Arc 包裹，trait 要求 Send + Sync |
| `id_generator` | `Arc<dyn IdGenerator>` | Arc 包裹，trait 要求 Send + Sync |
| `event_tx` | `Option<mpsc::Sender<...>>` | mpsc::Sender 是 Clone + Send |
| `sub_graph_runner` | `Option<Arc<dyn SubGraphRunner>>` | Arc 包裹 |
| `node_executor_registry` | `Option<Arc<NodeExecutorRegistry>>` | Arc 包裹 |
| `template_functions` | `Option<Arc<HashMap<...>>>` | Arc 包裹的只读 HashMap |
| `security` | `Option<SecurityContext>` | 值类型，Clone 后独立 |

**结论**：
- 所有共享的服务都用 `Arc` 包裹
- RuntimeContext 本身用 `Arc` 包裹，多线程共享
- 节点只接收 `&RuntimeContext`，只读访问，无法修改
- **零成本抽象**：无需 Mutex，编译期保证线程安全

---

## 七、总结

### 7.1 核心价值

1. **统一的服务入口**：避免在每个节点中传递大量参数
2. **依赖注入容器**：支持测试时注入 Mock 实现
3. **配置传递载体**：将全局配置传递到执行链的每个环节
4. **扩展点机制**：支持插件系统和安全模块的扩展

### 7.2 设计优势

1. **类型安全**：通过 Rust 类型系统保证线程安全
2. **零成本抽象**：Arc + 不可变引用，无运行时开销
3. **可测试性**：支持注入 Fake 实现（FakeTimeProvider, FakeIdGenerator）
4. **可扩展性**：RuntimeExtensions 提供扩展点

### 7.3 生命周期总结

```
创建 → 配置 → 冻结 → 共享 → 销毁
 ↓      ↓      ↓      ↓      ↓
值类型  可变   Arc包装 只读   引用计数归零
```

RuntimeContext 是 xworkflow 架构的**脊柱**，贯穿整个工作流生命周期，为所有组件提供统一的服务和配置访问入口。
