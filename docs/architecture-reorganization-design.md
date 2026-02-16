# XWorkflow 架构重整技术设计（目录与模块）

**文档类型**: 架构重构设计（实施前）  
**状态**: Draft / 待实现  
**最后更新**: 2026年2月13日

---

## 1. 背景与问题陈述

当前仓库在功能上较完整，但目录与模块边界存在结构性问题，导致维护和扩展成本持续上升。典型信号如下：

1. 分层反转
- `src/core/dispatcher.rs` 依赖 `src/scheduler.rs` 中的 `ExecutionStatus`。
- `src/compiler/compiler.rs`、`src/compiler/runner.rs` 直接依赖 `scheduler` 中的函数和 gate。

2. 职责重复
- `core/` 与 `scheduler/` 同时维护 plugin/security gate（启动期与执行期职责交叠）。

3. 协议层耦合运行时
- `src/dsl/schema.rs`、`src/dsl/validation/` 依赖 `core::variable_pool` 与 `nodes::*` 具体实现类型。

4. 低内聚高耦合的大文件
- `src/core/dispatcher.rs`、`src/scheduler.rs`、`src/core/variable_pool.rs`、`src/nodes/data_transform.rs` 行数过大且跨领域逻辑混合。

5. 遗留噪音
- `src/dsl/validator.rs` 未纳入主验证链路。
- `src/plugin/` 为空目录。

这些问题不会立刻破坏功能，但会显著放大后续改动风险，尤其是安全策略、插件机制、节点类型扩展和编译路径优化。

---

## 2. 设计目标与非目标

### 2.1 目标

1. 建立单向依赖分层，消除 `core -> scheduler`、`compiler -> scheduler` 反向依赖。  
2. 将启动期编排与执行期内核解耦，统一 gate 语义。  
3. 让 DSL 层成为协议层，不直接依赖运行时细节实现。  
4. 将超大文件按领域拆分，降低单文件复杂度。  
5. 在可控范围内保留对外 API 兼容通道，支持渐进迁移。

### 2.2 非目标

1. 不改变 DSL 语义和执行结果。  
2. 不引入新的业务能力（仅重构结构）。  
3. 不在本设计阶段强制拆分为更多 workspace crate（先做 `src/` 内分层）。

---

## 3. 设计原则

1. Security First  
安全策略和校验流程必须先保真，再做抽象迁移。

2. Obviousness  
目录命名与模块边界应直接表达职责，不依赖隐式约定。

3. Compile-Time Clarity  
通过类型和模块可见性限制非法依赖方向，减少“看代码才知道能不能调”的情况。

4. Compatibility by Default  
公开 API 迁移提供过渡别名，避免一次性破坏下游。

---

## 4. 目标分层模型

重构后采用五层模型：

1. `api`
- 对外暴露的稳定入口（`WorkflowRunner`、`WorkflowHandle`、builder、公开状态类型）。

2. `application`
- 业务用例编排层（运行工作流、编译后运行、恢复、安全停止、启动期插件加载）。

3. `domain`
- 纯领域模型与通用类型（执行状态、选择器、输出模型、编译中间数据）。

4. `engine`
- 执行内核（dispatcher、event bus、runtime context/group、执行期 gates、子图运行器）。

5. `infrastructure`
- 可替换基础设施实现（plugin_system、sandbox、security、llm provider adapter）。

依赖方向（必须遵守）：

`api -> application -> domain`
`application -> engine`
`engine -> domain`
`infrastructure -> domain`
`application/engine -> infrastructure`（通过 trait/port，避免反向）

禁止：

- `domain` 依赖 `engine`、`application`、`infrastructure`。  
- `engine` 依赖 `api`。  
- `compiler`（重构后归入 `application`）依赖 `api`。

---

## 5. 目标目录布局

```text
src/
  api/
    mod.rs
    runner.rs
    handle.rs
  application/
    mod.rs
    workflow_run/
      mod.rs
      run_schema.rs
      run_compiled.rs
      resume.rs
      safe_stop.rs
    bootstrap/
      mod.rs
      plugin_bootstrap.rs
      security_bootstrap.rs
    compile/
      mod.rs
      compiler.rs
      compiled_artifact.rs
  domain/
    mod.rs
    execution/
      mod.rs
      status.rs
      output.rs
    model/
      mod.rs
      selector.rs
      segment_type.rs
      workflow_types.rs
    compile/
      mod.rs
      typed_config.rs
  engine/
    mod.rs
    dispatcher/
      mod.rs
      runtime.rs
      command.rs
      event_emitter.rs
    runtime/
      mod.rs
      context.rs
      group.rs
      variable_pool.rs
    gates/
      mod.rs
      plugin_runtime_gate.rs
      security_runtime_gate.rs
    subgraph/
      mod.rs
      runner.rs
  infrastructure/
    mod.rs
    plugin/
    sandbox/
    security/
    llm/
  dsl/
    parser.rs
    schema.rs
    validation/
```


---

## 6. 关键设计决策

### 6.1 `ExecutionStatus` 上移到 `domain`

现状问题：
- 执行状态定义在 `scheduler`，但 `dispatcher` 需要使用，造成 `core -> scheduler`。

决策：
- 新增 `domain::execution::ExecutionStatus` 作为唯一真源。
- `api` 层 re-export 该类型，保留现有外部访问路径。


### 6.2 Gate 体系按生命周期分层

将 gate 明确拆为两类：

1. 启动期 gate（`application`）
- DSL 校验扩展、插件加载、运行前上下文注入。

2. 执行期 gate（`engine`）
- 节点执行前后安全控制、hook 调用、变量写拦截。

决策：
- `scheduler/plugin_gate.rs` 与 `scheduler/security_gate.rs` 迁移到 `application/bootstrap/*`。
- `core/plugin_gate.rs` 与 `core/security_gate.rs` 保留在 `engine/gates/*`。
- 消除重叠方法，避免双份策略。

### 6.3 编译与运行链路去耦

现状问题：
- `compiler` 直接依赖 `scheduler` 的收集函数和 gate 工厂。

决策：
- 类型收集函数迁入 `domain::compile` 或 `application::compile::mapping`。
- 编译后运行 builder 迁入 `application::workflow_run::run_compiled`。
- `application` 对外提供统一入口，内部根据 schema/compiled 分支。

### 6.4 DSL 层去运行时实现依赖

现状问题：
- DSL schema/validation 依赖 `core::variable_pool::{Segment, Selector, SegmentType}` 与 `nodes::subgraph_nodes::*`。

决策：
- 抽取协议稳定类型到 `domain::model`（如 `Selector`、`SegmentType` 的协议视图）。
- DSL 只依赖 `domain` 的中立类型，不直接引用 `engine`/`nodes` 具体实现。
- 节点特定 typed config 的解析逻辑迁入 `domain::compile` 或 `application::compile`。

### 6.5 节点模块按能力拆分

目标：
- 解决 `nodes/data_transform.rs`、`nodes/subgraph_nodes.rs` 巨型文件。

目标分包：

1. `nodes/control`
- start/end/answer/if-else/human-input

2. `nodes/transform`
- template/aggregator/assigner/http/code

3. `nodes/ai`
- llm/question-classifier/agent/tool

4. `nodes/flow`
- iteration/loop/list-operator/subgraph

约束：
- 每个节点执行器和其专用 config/parser 放在同一子模块目录。
- 优先将节点 typed config 与执行器共置，降低跨文件跳转成本。

---

## 7. 公共接口与类型变更

### 7.1 公开 API 变更（对外）

1. 类型来源变化
- `ExecutionStatus` 源头从 `scheduler` 迁至 `domain`，对外路径保持兼容导出。

2. Builder 路径变化
- 未来统一对外入口到 `api::runner`；短期保留 `WorkflowRunner::builder` 兼容。

### 7.2 内部接口变更（实现层）

1. 移除 `core` 对 `scheduler` 的引用。  
2. 移除 `compiler` 对 `scheduler` 的引用。  
3. DSL 验证器改为依赖 `domain` 中立类型，不直接拿节点实现类型。  
4. Gate 接口按启动期/执行期拆分，避免单 trait 过载。

---

## 8. 迁移计划（分阶段）

### 阶段 A：骨架与兼容层

产出：
1. 新增 `api/application/domain/engine/infrastructure` 骨架。  
2. 添加 re-export 兼容，不变更行为。  
3. 文档与索引同步。

验收：
1. `cargo check --all-features` 通过。  
2. 公开 API 无破坏性回归。

### 阶段 B：反向依赖清零

产出：
1. `ExecutionStatus`、类型收集函数迁移。  
2. `core/dispatcher` 不再 `use crate::scheduler::*`。  
3. `compiler/*` 不再 `use crate::scheduler::*`。

验收：
1. 代码搜索无上述反向依赖。  
2. 单元测试和集成测试通过。

### 阶段 C：Gate 合并重构

产出：
1. 启动期 gate 实现归 `application/bootstrap`。  
2. 执行期 gate 实现归 `engine/gates`。  
3. 删除重复逻辑与重复测试。

验收：
1. plugin/security 行为与现状一致。  
2. 默认 feature 与最小 feature 都能编译。

### 阶段 D：DSL 去耦

产出：
1. DSL 不再依赖 `core::variable_pool` 和节点具体实现。  
2. 引入 domain 中立协议类型。

验收：
1. `src/dsl` 下搜索不出现 `use crate::core::`、`use crate::nodes::`。  
2. 三层验证测试保持通过。

### 阶段 E：节点拆分与清理

产出：
1. 拆分 `nodes` 巨型文件。  
2. 清理 `src/dsl/validator.rs` 遗留与空目录 `src/plugin`。

验收：
1. 主要文件控制在可维护范围（建议 < 800 行）。  
2. 节点注册与执行路径保持一致。

---

## 9. 测试与验收策略

### 9.1 必跑命令

```bash
cargo test --all-features --workspace
cargo test --all-features --workspace --test integration_tests
cargo test --all-features --workspace --test memory_tests -- --test-threads=1
cargo check --workspace --all-targets --all-features
```

### 9.2 定向回归点

1. 插件生命周期
- bootstrap / normal phase、hook 注入、plugin shutdown。

2. 安全策略
- DSL 校验策略、资源配额、审计日志、输出大小限制。

3. 运行控制
- pause/resume、safe_stop、checkpoint（feature 开启时）。

4. AI 节点链路
- llm/question-classifier/agent/tool 的分支与错误策略。

### 9.3 依赖方向守卫

新增 CI 检查（可用脚本或 lint 工具）：

1. `domain` 不得依赖 `engine/application/infrastructure`。  
2. `engine` 不得依赖 `api`。  
3. `dsl` 不得依赖 `engine` 和 `nodes` 具体实现。

---

## 10. 风险与回滚策略

### 10.1 主要风险

1. 迁移过程中 feature gate 不对称，导致某些 feature 组合编译失败。  
2. Gate 合并时行为漂移，引入安全策略回归。  
3. DSL 去耦时若抽象过度，增加运行时转换成本。

### 10.2 缓解措施

1. 每阶段结束执行全特性测试。  
2. 保留行为对照测试（before/after 同一用例输出一致）。  
3. 关键路径先“移动不改逻辑”，再小步重构。

---

## 11. 实施默认假设

1. 本次重构允许目录和内部 API 大幅调整。  
3. 以“不改变行为语义”为硬约束，优先做结构治理。  
4. 文档先行，代码分阶段落地。.

---

## 12. 里程碑与完成定义

### M1：边界建模完成
- 新目录骨架与兼容层上线。
- 反向依赖清单形成并可追踪。

### M2：依赖方向完成
- `core`、`compiler` 不再依赖 `scheduler`。
- gate 生命周期分层完成。

### M3：可维护性完成
- DSL 去耦完成。
- 节点大文件拆分完成。
- 遗留模块清理完成。

完成定义（Definition of Done）：
1. 所有必跑测试通过。  
2. 依赖方向检查通过。  
3. 文档索引与状态总结同步更新。  
4. 无新增编译 warning。

