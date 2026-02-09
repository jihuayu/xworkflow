# E2E 测试框架设计方案

## Context

xworkflow 当前只有嵌入在源文件中的单元测试（47个），缺少端到端集成测试。需要设计一套基于文件夹的 e2e 测试框架，每个测试用例由 `workflow.json`、`in.json`、`state.json`、`out.json` 四个文件组成，测试运行器自动发现并执行所有用例。

核心挑战：引擎当前存在不确定性来源（UUID、系统时间），需要引入抽象层使测试可确定性运行。

---

## 一、引入 RuntimeContext 抽象（引擎改造）

### 1.1 新增 `src/core/runtime_context.rs`

```rust
use std::sync::atomic::{AtomicU64, Ordering};

/// 运行时上下文，提供可替换的时间、随机数等环境依赖
pub struct RuntimeContext {
    pub time_provider: Box<dyn TimeProvider>,
    pub id_generator: Box<dyn IdGenerator>,
}

pub trait TimeProvider: Send + Sync {
    fn now_timestamp(&self) -> i64;
    fn now_millis(&self) -> i64;
    fn elapsed_secs(&self, since: i64) -> u64;
}

pub trait IdGenerator: Send + Sync {
    fn next_id(&self) -> String;
}

// --- 真实实现（生产用）---
pub struct RealTimeProvider { start: std::time::Instant }
pub struct RealIdGenerator;

// --- 假实现（测试用）---
pub struct FakeTimeProvider { fixed_timestamp: i64 }
pub struct FakeIdGenerator { prefix: String, counter: AtomicU64 }
```

`Default for RuntimeContext` 使用真实实现，确保向后兼容。

### 1.2 修改 NodeExecutor trait（贯穿到节点层）

```rust
// src/nodes/executor.rs
#[async_trait]
pub trait NodeExecutor: Send + Sync {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,       // ← 新增参数
    ) -> Result<NodeRunResult, NodeError>;
}
```

**影响范围**：所有 NodeExecutor 实现都需要更新签名（约 15 个文件/结构体），但大多数只需在参数列表加 `_context: &RuntimeContext`。

### 1.3 Scheduler 改为 Builder 模式

```rust
// src/scheduler.rs
pub struct WorkflowRunner {
    schema: WorkflowSchema,
    user_inputs: HashMap<String, Value>,
    system_vars: HashMap<String, Value>,
    config: EngineConfig,
    context: RuntimeContext,
}

impl WorkflowRunner {
    pub fn builder(schema: WorkflowSchema) -> WorkflowRunnerBuilder { ... }
}

pub struct WorkflowRunnerBuilder { ... }

impl WorkflowRunnerBuilder {
    pub fn user_inputs(mut self, inputs: HashMap<String, Value>) -> Self;
    pub fn system_vars(mut self, vars: HashMap<String, Value>) -> Self;
    pub fn config(mut self, config: EngineConfig) -> Self;
    pub fn context(mut self, context: RuntimeContext) -> Self;
    pub async fn run(self) -> Result<WorkflowHandle, WorkflowError>;
}
```

**不保留旧 API**：删除 `WorkflowScheduler`，统一使用 `WorkflowRunner` builder。现有调用方（main.rs、examples、单元测试）全部迁移。

### 1.4 修改文件清单

| 文件 | 改动 |
|------|------|
| `src/core/runtime_context.rs` | **新增**：RuntimeContext + traits + Real/Fake 实现 |
| `src/core/mod.rs` | 导出 runtime_context 模块 |
| `src/core/dispatcher.rs` | 持有 `Arc<RuntimeContext>`，替换 `Uuid::new_v4()` → `context.id_generator.next_id()`，替换 `Instant::now()` → `context.time_provider` |
| `src/nodes/executor.rs` | NodeExecutor trait 新增 `context` 参数 |
| `src/nodes/control_flow.rs` | 更新所有 executor 签名 |
| `src/nodes/data_transform.rs` | 更新所有 executor 签名 |
| `src/nodes/subgraph.rs` | 更新 executor 签名 |
| `src/nodes/subgraph_nodes.rs` | 更新 executor 签名 |
| `src/scheduler.rs` | 删除 `WorkflowScheduler`，替换为 `WorkflowRunner` builder |
| `src/lib.rs` | 导出 RuntimeContext 相关类型，更新 scheduler 导出 |
| `src/main.rs` | 迁移到 WorkflowRunner builder API |
| `examples/scheduler_demo.rs` | 迁移到 WorkflowRunner builder API |

---

## 二、测试文件结构

```
tests/
  e2e/
    mod.rs                   # 模块声明
    runner.rs                # TestCase 加载/执行/比较逻辑
    macros.rs                # 宏：为每个用例文件夹生成独立 #[tokio::test]
    cases/
      001_simple_passthrough/
        workflow.json
        in.json
        state.json
        out.json
      002_ifelse_true_branch/
        ...
      003_template_transform/
        ...
tests/
  e2e_tests.rs              # 集成测试入口，调用宏展开
```

---

## 三、各文件 JSON Schema

### 3.1 `workflow.json`
标准 `WorkflowSchema` JSON，与 `parse_dsl(content, DslFormat::Json)` 兼容。

### 3.2 `in.json`
```json
{ "query": "hello world", "count": 42 }
```

### 3.3 `state.json`
```json
{
  "config": {
    "max_steps": 500,
    "max_execution_time_secs": 60
  },
  "system_variables": {
    "user_id": "test-user-001"
  },
  "environment_variables": {},
  "conversation_variables": {},
  "fake_time": { "fixed_timestamp": 1700000000 },
  "fake_id": { "prefix": "test" }
}
```
所有字段均可选，缺省使用默认值。

### 3.4 `out.json`
```json
{
  "status": "completed",
  "outputs": { "result": "hello world" },
  "partial_match": false
}
```
或失败预期：
```json
{
  "status": "failed",
  "error_contains": "Max steps exceeded"
}
```

---

## 四、测试运行器实现

### 4.1 宏生成独立测试（`tests/e2e/macros.rs`）

```rust
/// 为指定目录下每个子文件夹生成一个独立的 #[tokio::test]
macro_rules! e2e_test_cases {
    ($dir:expr, $($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                let case_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join($dir)
                    .join(stringify!($name));
                crate::e2e::runner::run_case(&case_dir).await;
            }
        )*
    };
}
```

### 4.2 `tests/e2e_tests.rs` 入口

```rust
mod e2e;

e2e::e2e_test_cases!("tests/e2e/cases",
    001_simple_passthrough,
    002_ifelse_true_branch,
    003_template_transform,
);
```

每新增一个用例文件夹，只需在宏调用中加一行。每个用例是独立的 `#[tokio::test]`，可并行运行，失败定位精确。

### 4.3 `tests/e2e/runner.rs` 核心逻辑

```rust
pub async fn run_case(case_dir: &Path) {
    // 1. 读取 4 个 JSON 文件
    // 2. parse_dsl → WorkflowSchema
    // 3. 从 state.json 构建 EngineConfig + RuntimeContext
    // 4. WorkflowRunner::builder(schema)
    //      .user_inputs(inputs)
    //      .system_vars(sys_vars)
    //      .config(config)
    //      .context(context)
    //      .run().await
    // 5. handle.wait().await → ExecutionStatus
    // 6. 与 out.json 比较，assert 失败时输出详细 diff
}
```

比较逻辑：
- `Completed` vs `Completed`：精确匹配或 `partial_match` 模式
- `Failed` vs `Failed`：检查 `error_contains` 子串
- 状态不匹配：panic 并输出详细信息

---

## 五、初始测试用例

| 用例 | 工作流 | 验证点 |
|------|--------|--------|
| `001_simple_passthrough` | start → end | 输入直传输出 |
| `002_ifelse_true_branch` | start → if-else → end_a/end_b | 条件为真走 true 分支 |
| `003_template_transform` | start → template → end | Jinja 模板渲染 |

---

## 六、实施步骤

1. **新增 `src/core/runtime_context.rs`**：RuntimeContext + traits + Real/Fake 实现
2. **修改 NodeExecutor trait**：`execute()` 新增 `context: &RuntimeContext` 参数
3. **更新所有 NodeExecutor 实现**：control_flow、data_transform、subgraph、subgraph_nodes、StubExecutor
4. **修改 `src/core/dispatcher.rs`**：注入 RuntimeContext，替换 UUID 和时间调用
5. **重写 `src/scheduler.rs`**：删除 WorkflowScheduler，实现 WorkflowRunner builder
6. **更新 `src/core/mod.rs` 和 `src/lib.rs`**：导出新类型
7. **迁移 `src/main.rs` 和 `examples/scheduler_demo.rs`**：使用新 API
8. **新增 `tests/e2e/` 目录**：runner.rs + macros.rs + 3 个初始用例
9. **新增 `tests/e2e_tests.rs`**：宏展开入口
10. **运行验证**：`cargo test` 确保现有单元测试不受影响
11. **运行验证**：`cargo test --test e2e_tests` 确保 3 个 e2e 用例通过

---

## 七、验证方式

```bash
# 全量测试（单元 + e2e）
cargo test

# 仅 e2e
cargo test --test e2e_tests

# 单个用例
cargo test --test e2e_tests 001_simple_passthrough
```
