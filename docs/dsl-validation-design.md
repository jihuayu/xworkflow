# DSL 合法性检查接口设计

## Context

xworkflow 当前的 DSL 校验（`src/dsl/validator.rs`）仅覆盖最基础的结构检查：版本号、Start/End 节点存在性、节点 ID 唯一性、边引用合法性。大量语义错误（环路、孤立节点、节点配置缺失、变量引用断链、分支边不完整等）只有在运行时才暴露，用户得不到清晰的前置反馈。

本方案设计一个全面的 DSL 合法性检查接口，在工作流执行前一次性收集所有问题，以结构化的诊断报告返回给调用方。

---

## 一、接口设计

### 1.1 核心类型

```rust
/// 诊断严重级别
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticLevel {
    /// 致命错误：工作流无法执行
    Error,
    /// 警告：工作流可执行但可能存在问题
    Warning,
}

/// 单条诊断信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// 严重级别
    pub level: DiagnosticLevel,
    /// 错误规则码，如 "E001"、"W001"
    pub code: String,
    /// 人类可读的错误描述
    pub message: String,
    /// 关联的节点 ID（可选）
    pub node_id: Option<String>,
    /// 关联的边 ID（可选）
    pub edge_id: Option<String>,
    /// 关联的字段路径，如 "cases[0].conditions[1].variable_selector"
    pub field_path: Option<String>,
}

/// 校验结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    /// 是否合法（无 Error 级别诊断）
    pub is_valid: bool,
    /// 所有诊断信息
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    pub fn errors(&self) -> Vec<&Diagnostic> {
        self.diagnostics.iter().filter(|d| d.level == DiagnosticLevel::Error).collect()
    }

    pub fn warnings(&self) -> Vec<&Diagnostic> {
        self.diagnostics.iter().filter(|d| d.level == DiagnosticLevel::Warning).collect()
    }
}
```

### 1.2 公共 API

```rust
/// 校验 DSL 字符串，返回结构化报告
pub fn validate_dsl(content: &str, format: DslFormat) -> ValidationReport

/// 校验已解析的 WorkflowSchema，返回结构化报告
pub fn validate_schema(schema: &WorkflowSchema) -> ValidationReport
```

`validate_dsl` 先尝试解析，解析失败则报告 `E001` 并立即返回；解析成功则调用 `validate_schema`。

`validate_schema` 按规则分层依次执行，**不提前返回**，收集所有可发现的问题。

### 1.3 在 WorkflowRunner 中的集成

```rust
impl WorkflowRunnerBuilder {
    /// 仅校验不执行，返回 ValidationReport
    pub fn validate(&self) -> ValidationReport

    /// 现有 run() 方法内部改为调用 validate_schema()，
    /// 如有 Error 级别诊断则返回 Err(WorkflowError::ValidationFailed(report))
}
```

### 1.4 序列化输出

`ValidationReport` 派生 `Serialize`，支持直接序列化为 JSON，方便前端消费：

```json
{
  "is_valid": false,
  "diagnostics": [
    {
      "level": "Error",
      "code": "E003",
      "message": "Cycle detected: start -> code1 -> code2 -> start",
      "node_id": "start",
      "edge_id": null,
      "field_path": null
    },
    {
      "level": "Warning",
      "code": "W001",
      "message": "Node 'unused_code' is unreachable from start node",
      "node_id": "unused_code",
      "edge_id": null,
      "field_path": null
    }
  ]
}
```

---

## 二、校验规则清单

校验按依赖关系分为三层，依次执行。每层内的规则彼此独立，可并行收集诊断。

### Layer 1：结构校验（Schema-level）

无需构建图，仅基于 `WorkflowSchema` 数据。

| 规则码 | 级别 | 规则名 | 说明 |
|--------|------|--------|------|
| E001 | Error | `parse_error` | DSL 内容无法解析为合法 YAML/JSON |
| E002 | Error | `unsupported_version` | `version` 不在 `SUPPORTED_DSL_VERSIONS` 中 |
| E003 | Error | `empty_nodes` | `nodes` 为空数组 |
| E004 | Error | `no_start_node` | 没有 `type: start` 节点 |
| E005 | Error | `multiple_start_nodes` | 存在多个 `type: start` 节点 |
| E006 | Error | `no_end_or_answer_node` | 没有 `type: end` 或 `type: answer` 节点 |
| E007 | Error | `duplicate_node_id` | 存在重复的节点 ID |
| E008 | Error | `empty_node_id` | 节点 ID 为空字符串 |
| E009 | Error | `unknown_node_type` | `node_type` 不在 `NodeType` 枚举中（已知类型列表）|
| E010 | Error | `edge_source_not_found` | 边的 `source` 引用了不存在的节点 ID |
| E011 | Error | `edge_target_not_found` | 边的 `target` 引用了不存在的节点 ID |
| E012 | Error | `self_loop_edge` | 边的 `source` 和 `target` 是同一个节点 |
| E013 | Error | `duplicate_edge` | 两条边具有相同的 (source, target, source_handle) |
| W001 | Warning | `node_missing_title` | 节点 `title` 为空字符串 |

### Layer 2：图拓扑校验（Graph-level）

基于 Layer 1 收集的节点和边信息构建邻接表，执行图算法。如果 Layer 1 有致命结构错误（如节点为空），则跳过此层。

| 规则码 | 级别 | 规则名 | 说明 |
|--------|------|--------|------|
| E101 | Error | `cycle_detected` | 图中存在环路（DFS 检测），message 中附带环路路径 |
| E102 | Error | `unreachable_node` | 从 start 节点 BFS 无法到达的节点 |
| E103 | Error | `no_path_to_end` | 从 start 节点无法到达任何 end/answer 节点 |
| E104 | Error | `start_has_incoming_edges` | start 节点有入边 |
| W101 | Warning | `end_has_outgoing_edges` | end/answer 节点有出边 |
| W102 | Warning | `orphan_edge` | 边引用的节点虽存在但该边不在任何有效路径上 |

### Layer 3：语义校验（Semantic-level）

逐节点解析 `NodeData.extra` 为各节点类型的强类型配置（如 `IfElseNodeData`、`CodeNodeData`），校验节点间的语义关联。如果 Layer 1 中 `E009`（未知节点类型）命中，则跳过对应节点。

#### 3.1 节点配置校验

| 规则码 | 级别 | 适用节点类型 | 规则名 | 说明 |
|--------|------|-------------|--------|------|
| E201 | Error | start | `start_invalid_var_type` | 变量的 `type` 不在合法类型列表 (`string`, `number`, `object`, `array[string]`, `array[number]`, `array[object]`, `file`, `array[file]`) 中 |
| E202 | Error | end | `end_empty_value_selector` | `outputs` 中的 `value_selector` 为空数组 |
| E203 | Error | answer | `answer_empty_template` | `answer` 模板字符串为空 |
| E204 | Error | if-else | `ifelse_empty_cases` | `cases` 为空数组 |
| E205 | Error | if-else | `ifelse_empty_conditions` | 某个 case 的 `conditions` 为空数组 |
| E206 | Error | if-else | `ifelse_duplicate_case_id` | 同一 if-else 节点内 `case_id` 重复 |
| E207 | Error | code | `code_empty_code` | `code` 字段为空字符串 |
| E208 | Error | code | `code_unsupported_language` | `language` 不是 `javascript` 或 `python3` |
| E209 | Error | http-request | `http_empty_url` | `url` 字段为空字符串 |
| E210 | Error | template-transform | `template_empty` | `template` 字段为空字符串 |
| E211 | Error | iteration | `iteration_missing_sub_graph` | 迭代节点缺少子图定义（`sub_graph` 字段）|
| E212 | Error | iteration | `iteration_empty_iterator` | `iterator_selector` 为空数组 |
| E213 | Error | loop | `loop_missing_sub_graph` | 循环节点缺少子图定义 |
| E214 | Error | loop | `loop_missing_break_condition` | 循环节点缺少终止条件 |
| E215 | Error | variable-aggregator | `aggregator_empty_variables` | `variables` 为空数组 |
| E216 | Error | list-operator | `list_op_missing_operation` | 缺少 `operation` 字段 |
| E217 | Error | * | `config_parse_error` | 节点的 `extra` 字段无法反序列化为对应的强类型配置 |
| W201 | Warning | code | `code_python_unsupported` | `language: python3` 当前运行时不支持 |
| W202 | Warning | * (stub) | `stub_node_type` | 使用了 stub 节点类型（如 `llm`、`knowledge-retrieval` 等未完整实现的类型），运行时将返回占位结果 |

#### 3.2 分支边完整性校验

| 规则码 | 级别 | 规则名 | 说明 |
|--------|------|--------|------|
| E301 | Error | `branch_missing_false_edge` | if-else 节点缺少 `sourceHandle: "false"` 的出边（默认分支）|
| E302 | Error | `branch_missing_case_edge` | if-else 节点的某个 `case_id` 没有对应的出边 |
| E303 | Error | `non_branch_has_source_handle` | 非分支节点（非 if-else/question-classifier）的出边设置了 `source_handle` 且值不是 `"source"` |
| E304 | Error | `branch_edge_unknown_handle` | 分支节点的出边 `source_handle` 既不是已声明的 `case_id` 也不是 `"false"` |
| W301 | Warning | `branch_extra_edge` | 分支节点有出边的 `source_handle` 未匹配任何 case（多余的边）|

#### 3.3 变量引用校验

校验 `VariableSelector` 引用的合法性。变量选择器格式为 `["node_id", "output_name"]` 或 `["sys", "key"]` / `["env", "key"]` / `["conversation", "key"]`。

| 规则码 | 级别 | 规则名 | 说明 |
|--------|------|--------|------|
| E401 | Error | `selector_too_short` | `variable_selector` 长度 < 2 |
| E402 | Error | `selector_ref_unknown_node` | 选择器的第一个元素不是已知节点 ID，也不是保留命名空间 (`sys`, `env`, `conversation`) |
| E403 | Error | `selector_ref_downstream_node` | 选择器引用了 DAG 中位于当前节点下游的节点（执行时尚未产生输出）|
| W401 | Warning | `selector_ref_unreachable_node` | 选择器引用的节点存在但可能因分支跳过而无输出 |

适用位置（需遍历检查的字段）：
- `EndNodeData.outputs[*].value_selector`
- `IfElseNodeData.cases[*].conditions[*].variable_selector`
- `TemplateTransformNodeData.variables[*].value_selector`
- `CodeNodeData.variables[*].value_selector`
- `VariableAggregatorNodeData.variables[*]`
- `VariableAssignerNodeData.assigned_variable_selector`
- `VariableAssignerNodeData.input_variable_selector`
- `IterationNodeData.iterator_selector`
- `IterationNodeData.output_selector`

#### 3.4 模板语法校验

| 规则码 | 级别 | 规则名 | 说明 |
|--------|------|--------|------|
| E501 | Error | `jinja2_syntax_error` | Jinja2 模板语法错误（通过 `minijinja` 试编译检测）|
| E502 | Error | `dify_template_malformed` | Dify 模板 `{{#...#}}` 内的选择器格式错误（不包含 `.` 分隔符）|
| W501 | Warning | `jinja2_undefined_var` | Jinja2 模板引用的变量名在 `variables` 映射中不存在 |
| W502 | Warning | `dify_template_ref_unknown` | Dify 模板 `{{#node_id.output#}}` 引用的节点不存在 |

适用位置：
- `TemplateTransformNodeData.template` → Jinja2 语法
- `AnswerNodeData.answer` → Dify `{{#...#}}` 语法
- `HttpRequestNodeData.url` / `headers[*].value` / `params[*].value` → Dify `{{#...#}}` 语法

---

## 三、校验流程

```
validate_dsl(content, format)
│
├─ 1. 解析 DSL
│     失败 → 返回 [E001]
│     成功 → WorkflowSchema
│
└─ validate_schema(schema)
   │
   ├─ 2. Layer 1: 结构校验
   │     E002~E013, W001
   │     如果有 E003/E004/E005 → 跳过 Layer 2
   │
   ├─ 3. Layer 2: 图拓扑校验
   │     构建邻接表
   │     E101~E104, W101~W102
   │
   ├─ 4. Layer 3: 语义校验
   │     3.1 节点配置: E201~E217, W201~W202
   │     3.2 分支边: E301~E304, W301
   │     3.3 变量引用: E401~E403, W401
   │     3.4 模板语法: E501~E502, W501~W502
   │
   └─ 5. 汇总
         is_valid = (errors 数量 == 0)
         返回 ValidationReport
```

---

## 四、文件结构

```
src/dsl/
├── mod.rs                  # 删除 pub mod validator，替换为 pub mod validation
├── parser.rs               # 不变
├── schema.rs               # 不变
├── validator.rs            # 删除
└── validation/
    ├── mod.rs              # validate_dsl(), validate_schema()
    ├── types.rs            # DiagnosticLevel, Diagnostic, ValidationReport
    ├── layer1_structure.rs # Layer 1 结构校验规则
    ├── layer2_topology.rs  # Layer 2 图拓扑校验规则
    ├── layer3_semantic.rs  # Layer 3 语义校验（节点配置 + 分支边 + 变量引用 + 模板）
    └── known_types.rs      # 已知节点类型列表、保留命名空间等常量
```

### 对现有代码的改动

- `src/dsl/validator.rs`：**删除**，全部逻辑移入 `validation/` 模块。
- `src/dsl/mod.rs`：`pub mod validator` → `pub mod validation`。
- `src/error/workflow_error.rs`：新增 `ValidationFailed(ValidationReport)` 变体。
- `src/lib.rs`：新增导出 `validate_dsl`, `validate_schema`, `ValidationReport`, `Diagnostic`, `DiagnosticLevel`。
- `src/scheduler.rs`：
  - `WorkflowRunnerBuilder` 新增 `validate()` 方法。
  - `run()` 内部原来调用 `validate_workflow_schema()` 的地方，改为调用 `validate_schema()`，如有 Error 级别诊断则返回 `Err(WorkflowError::ValidationFailed(report))`。

---

## 五、关键算法

### 5.1 环路检测（E101）

三色 DFS（White/Gray/Black）。当访问到 Gray 节点时，回溯 Gray 栈得到环路路径，写入 `message`。

```
fn detect_cycles(nodes, out_edges) -> Vec<Diagnostic>
```

### 5.2 可达性分析（E102, E103）

从 start 节点 BFS，标记所有可达节点。未标记节点报告 `E102`。如果可达节点集合中不包含任何 end/answer 节点，报告 `E103`。

```
fn check_reachability(root_id, nodes, out_edges) -> Vec<Diagnostic>
```

### 5.3 变量引用上游检查（E403）

利用 BFS/DFS 生成拓扑序，对每个节点记录其拓扑层级。如果变量选择器引用的节点拓扑层级 >= 当前节点的拓扑层级，则为引用了下游节点。

```
fn check_variable_references(schema, topo_order) -> Vec<Diagnostic>
```

---

## 六、使用方式

### Rust API

```rust
use xworkflow::{validate_dsl, DslFormat};

let yaml = std::fs::read_to_string("workflow.yaml")?;
let report = validate_dsl(&yaml, DslFormat::Yaml);

if report.is_valid {
    println!("DSL is valid");
} else {
    for diag in &report.diagnostics {
        println!("[{}] {}: {}", diag.code, diag.level, diag.message);
        if let Some(node) = &diag.node_id {
            println!("  at node: {}", node);
        }
    }
}
```

### 集成到 WorkflowRunner

```rust
let report = WorkflowRunner::builder(schema)
    .user_inputs(inputs)
    .validate();

if !report.is_valid {
    // 展示错误给用户，不执行
    return Err(report);
}

// 校验通过，执行
let handle = WorkflowRunner::builder(schema)
    .user_inputs(inputs)
    .run()
    .await?;
```

### JSON 输出（给前端）

```rust
let report = validate_dsl(&yaml, DslFormat::Yaml);
let json = serde_json::to_string_pretty(&report)?;
```

---

## 七、测试策略

每条规则至少一个正向测试（触发）和一个反向测试（不触发）：

1. **Layer 1 单元测试**：构造最小 `WorkflowSchema`，逐条验证 E002~E013
2. **Layer 2 单元测试**：
   - 构造含环路的图，验证 E101 报告正确路径
   - 构造含孤立节点的图，验证 E102
   - 构造无法到达 end 的图，验证 E103
3. **Layer 3 单元测试**：
   - 每种节点类型的配置缺失/非法场景
   - 分支节点边完整性的各种缺失场景
   - 变量选择器引用上游/下游/不存在节点
   - Jinja2 和 Dify 模板语法错误
4. **集成测试**：使用现有 `tests/integration/cases/` 中的合法 DSL 文件，确保全部 `is_valid == true`
