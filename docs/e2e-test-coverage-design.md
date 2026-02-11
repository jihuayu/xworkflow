# E2E 测试覆盖率提升设计文档

## 背景

当前项目有 78 个 e2e 测试用例（001-078），但以下关键代码路径缺乏覆盖：

| 模块 | 文件 | 现有 e2e 测试数 |
|------|------|-----------------|
| Iteration 节点 | `src/nodes/subgraph_nodes.rs` IterationNodeExecutor | 0 |
| Loop 节点 | `src/nodes/subgraph_nodes.rs` LoopNodeExecutor | 0 |
| List Operator 节点 | `src/nodes/subgraph_nodes.rs` ListOperatorNodeExecutor (12种操作) | 0 |
| Variable Assigner 写入模式 | `src/nodes/data_transform.rs` VariableAssignerExecutor | 0（仅 LLM streaming 间接覆盖） |
| 错误策略 FailBranch | `src/core/dispatcher.rs` 868-875行 | 0 |
| 错误策略 DefaultValue | `src/core/dispatcher.rs` 877-889行 | 0 |
| SubGraph Runner | `src/core/sub_graph_runner.rs` | 0（仅 error_handler 间接覆盖） |
| 环境变量在工作流中使用 | `src/scheduler.rs` 636-639行 | 0 |
| 会话变量 | `src/scheduler.rs` 642-646行 | 0 |
| 菱形 DAG 合并 | `src/graph/types.rs` is_node_ready | 0 |

## 目标

新增 22 个 e2e 测试用例（编号 079-100），覆盖以上所有空白路径。

## 测试框架说明

### 目录结构

每个测试用例是 `tests/integration/cases/` 下的一个目录，包含 4 个 JSON 文件：

```
tests/integration/cases/079_iteration_sequential/
├── workflow.json   # 工作流 DSL 定义
├── in.json         # 用户输入变量
├── out.json        # 预期输出（status, outputs, partial_match, error_contains）
└── state.json      # 测试配置（mock_server, fake_time, environment_variables, conversation_variables 等）
```

### 注册测试

在 `tests/integration_tests.rs` 的 `e2e_test_cases!` 宏中添加条目：

```rust
// 在现有 case_074 之后添加
case_079_iteration_sequential => "079_iteration_sequential",
case_080_iteration_parallel => "080_iteration_parallel",
case_081_iteration_error_terminated => "081_iteration_error_terminated",
case_082_iteration_error_remove_abnormal => "082_iteration_error_remove_abnormal",
case_083_iteration_error_continue => "083_iteration_error_continue",
case_084_iteration_max_exceeded => "084_iteration_max_exceeded",
case_085_loop_basic => "085_loop_basic",
case_086_loop_max_exceeded => "086_loop_max_exceeded",
case_087_loop_type_mismatch_break => "087_loop_type_mismatch_break",
case_088_list_op_simple => "088_list_op_simple",
case_089_list_op_sort_slice => "089_list_op_sort_slice",
case_090_list_op_filter => "090_list_op_filter",
case_091_list_op_map => "091_list_op_map",
case_092_list_op_reduce => "092_list_op_reduce",
case_093_assigner_overwrite => "093_assigner_overwrite",
case_094_assigner_append => "094_assigner_append",
case_095_assigner_clear => "095_assigner_clear",
case_096_error_strategy_fail_branch => "096_error_strategy_fail_branch",
case_097_error_strategy_default_value => "097_error_strategy_default_value",
case_098_env_vars_in_workflow => "098_env_vars_in_workflow",
case_099_conversation_vars => "099_conversation_vars",
case_100_diamond_merge => "100_diamond_merge",
```

### out.json 格式

```json
{
  "status": "completed" | "failed" | "failed_with_recovery",
  "outputs": { "key": "value" },
  "partial_match": false,
  "error_contains": "可选的错误子串匹配"
}
```

### 子图 DSL 格式

子图节点使用 `SubGraphNode` 格式（与主工作流的 `NodeSchema` 不同）：

```json
{
  "nodes": [
    { "id": "sg_start", "type": "start", "title": "SG Start", "data": {} },
    {
      "id": "sg_code", "type": "code", "title": "Process",
      "data": {
        "code": "function main(inputs) { return { value: inputs.item * 2 }; }",
        "language": "javascript",
        "variables": [
          { "variable": "item", "value_selector": ["__scope__", "item"] }
        ]
      }
    },
    {
      "id": "sg_end", "type": "end", "title": "SG End",
      "data": {
        "outputs": [
          { "variable": "value", "value_selector": ["sg_code", "value"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "sg_start", "target": "sg_code" },
    { "source": "sg_code", "target": "sg_end" }
  ]
}
```

### 作用域变量选择器

| 上下文 | 作用域变量 | 选择器 |
|--------|-----------|--------|
| Iteration 子图 | item (当前元素) | `["__scope__", "item"]` |
| Iteration 子图 | index (当前索引) | `["__scope__", "index"]` |
| Loop 子图 | loop.counter 等 | `["loop", "counter"]` |
| Loop 子图 | _iteration (迭代次数) | `["__scope__", "_iteration"]` |
| List-Op Filter/Map 子图 | item | `["__scope__", "item"]` |
| List-Op Filter/Map 子图 | index | `["__scope__", "index"]` |
| List-Op Reduce 子图 | accumulator | `["__scope__", "accumulator"]` |
| 环境变量 | env.VAR_NAME | `["env", "VAR_NAME"]` |
| 会话变量 | conversation.name | `["conversation", "name"]` |

原理：`inject_scope_vars` 调用 `Selector::parse_str(key)`，单部分 key（如 `"item"`）解析为 `Selector::new("__scope__", "item")`，带点的 key（如 `"loop.counter"`）解析为 `Selector::new("loop", "counter")`。

---

## 第一阶段：Iteration 节点测试（079-084）

覆盖 `src/nodes/subgraph_nodes.rs` IterationNodeExecutor 和 `src/core/sub_graph_runner.rs`。

### 079_iteration_sequential

**目的**：测试顺序迭代，覆盖 `execute_sequential` 和完整的子图执行路径。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": {
        "type": "start",
        "title": "Start",
        "variables": [
          { "variable": "items", "label": "Items", "type": "array-number", "required": true }
        ]
      }
    },
    {
      "id": "iter1",
      "data": {
        "type": "iteration",
        "title": "Sequential Iteration",
        "input_selector": ["start", "items"],
        "parallel": false,
        "sub_graph": {
          "nodes": [
            { "id": "sg_start", "type": "start", "title": "SG Start", "data": {} },
            {
              "id": "sg_code", "type": "code", "title": "Double",
              "data": {
                "code": "function main(inputs) { return { value: inputs.item * 2 }; }",
                "language": "javascript",
                "variables": [
                  { "variable": "item", "value_selector": ["__scope__", "item"] }
                ]
              }
            },
            {
              "id": "sg_end", "type": "end", "title": "SG End",
              "data": {
                "outputs": [
                  { "variable": "value", "value_selector": ["sg_code", "value"] }
                ]
              }
            }
          ],
          "edges": [
            { "source": "sg_start", "target": "sg_code" },
            { "source": "sg_code", "target": "sg_end" }
          ]
        },
        "output_variable": "results"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "results", "value_selector": ["iter1", "results"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "iter1" },
    { "source": "iter1", "target": "end" }
  ]
}
```

**in.json**: `{ "items": [1, 2, 3] }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "results": [{"value": 2}, {"value": 4}, {"value": 6}] },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 080_iteration_parallel

**目的**：测试并行迭代，覆盖 `execute_parallel` 和 Semaphore 限流逻辑。

**workflow.json**: 与 079 相同，但 iter1 节点改为：
```json
{
  "type": "iteration",
  "title": "Parallel Iteration",
  "input_selector": ["start", "items"],
  "parallel": true,
  "parallelism": 2,
  "sub_graph": { /* 与 079 相同的子图 */ },
  "output_variable": "results"
}
```

**in.json**: `{ "items": [10, 20, 30, 40] }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "results": [{"value": 20}, {"value": 40}, {"value": 60}, {"value": 80}] },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 081_iteration_error_terminated

**目的**：测试 `IterationErrorMode::Terminated`（默认模式），子图执行失败时整个迭代中止。

**workflow.json**: 与 079 结构相同，但子图代码为：
```javascript
function main(inputs) {
  if (inputs.item === 0) throw new Error('divide by zero');
  return { value: inputs.item * 10 };
}
```
`error_handle_mode` 使用默认值（terminated），不需要显式设置。

**in.json**: `{ "items": [1, 0, 3] }`

**out.json**:
```json
{
  "status": "failed",
  "error_contains": "divide by zero"
}
```

**state.json**: `{}`

---

### 082_iteration_error_remove_abnormal

**目的**：测试 `IterationErrorMode::RemoveAbnormal`，失败的元素从结果中移除。

**workflow.json**: 与 081 相同，但 iter1 节点增加 `"error_handle_mode": "remove_abnormal"`。

**in.json**: `{ "items": [1, 0, 3] }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "results": [{"value": 10}, {"value": 30}] },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 083_iteration_error_continue

**目的**：测试 `IterationErrorMode::ContinueOnError`，失败的元素用 null 替代。

**workflow.json**: 与 081 相同，但 iter1 节点增加 `"error_handle_mode": "continue_on_error"`。

**in.json**: `{ "items": [1, 0, 3] }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "results": [{"value": 10}, null, {"value": 30}] },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 084_iteration_max_exceeded

**目的**：测试迭代节点的 `max_iterations` 限制，数组长度超过限制时报错。

**workflow.json**: 与 079 相同，但 iter1 节点增加 `"max_iterations": 2`，输入 3 个元素。

**in.json**: `{ "items": [1, 2, 3] }`

**out.json**:
```json
{
  "status": "failed",
  "error_contains": "exceeds max iterations"
}
```

**state.json**: `{}`

---

## 第二阶段：Loop 节点测试（085-087）

覆盖 `src/nodes/subgraph_nodes.rs` LoopNodeExecutor。

### 085_loop_basic

**目的**：测试基本的 while 循环，计数器从 0 增长到 3。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": { "type": "start", "title": "Start" }
    },
    {
      "id": "loop1",
      "data": {
        "type": "loop",
        "title": "Count Loop",
        "condition": {
          "variable_selector": ["loop", "counter"],
          "comparison_operator": "less_than",
          "value": 3
        },
        "max_iterations": 10,
        "initial_vars": { "counter": 0 },
        "sub_graph": {
          "nodes": [
            { "id": "sg_start", "type": "start", "title": "SG Start", "data": {} },
            {
              "id": "sg_code", "type": "code", "title": "Increment",
              "data": {
                "code": "function main(inputs) { return { counter: inputs.counter + 1 }; }",
                "language": "javascript",
                "variables": [
                  { "variable": "counter", "value_selector": ["loop", "counter"] }
                ]
              }
            },
            {
              "id": "sg_end", "type": "end", "title": "SG End",
              "data": {
                "outputs": [
                  { "variable": "counter", "value_selector": ["sg_code", "counter"] }
                ]
              }
            }
          ],
          "edges": [
            { "source": "sg_start", "target": "sg_code" },
            { "source": "sg_code", "target": "sg_end" }
          ]
        },
        "output_variable": "loop_result"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "loop_result", "value_selector": ["loop1", "loop_result"] },
          { "variable": "iterations", "value_selector": ["loop1", "_iterations"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "loop1" },
    { "source": "loop1", "target": "end" }
  ]
}
```

**in.json**: `{}`

**out.json**:
```json
{
  "status": "completed",
  "outputs": {
    "loop_result": { "counter": 3 },
    "iterations": 3
  },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 086_loop_max_exceeded

**目的**：测试 Loop 节点的 `max_iterations` 限制，条件永远为真导致超限。

**workflow.json**: 与 085 结构相同，改动：
- `condition.value`: 改为 `1000`（counter < 1000 永远为真）
- `max_iterations`: 改为 `3`

**in.json**: `{}`

**out.json**:
```json
{
  "status": "failed",
  "error_contains": "Max iterations 3 exceeded"
}
```

**state.json**: `{}`

---

### 087_loop_type_mismatch_break

**目的**：测试循环条件中的类型不匹配（数字与字符串比较），在非严格模式下优雅终止循环。

**workflow.json**: 与 085 结构相同，改动：
- `condition`: `{ "variable_selector": ["loop", "counter"], "comparison_operator": "greater_than", "value": "abc" }`
- `initial_vars`: `{ "counter": 5 }`

期望：类型不匹配导致 `ConditionResult::TypeMismatch`，非严格模式下循环直接 break，循环体从未执行。

**in.json**: `{}`

**out.json**:
```json
{
  "status": "completed",
  "outputs": {
    "loop_result": { "counter": 5 },
    "iterations": 0
  },
  "partial_match": false
}
```

**state.json**: `{}`

---

## 第三阶段：List Operator 测试（088-092）

覆盖 `src/nodes/subgraph_nodes.rs` ListOperatorNodeExecutor 的 12 种操作。

### 088_list_op_simple

**目的**：一次覆盖 5 种简单操作：unique、reverse、first、last、length。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": {
        "type": "start",
        "title": "Start",
        "variables": [
          { "variable": "items", "label": "Items", "type": "array-number", "required": true }
        ]
      }
    },
    {
      "id": "op_unique",
      "data": {
        "type": "list-operator",
        "title": "Unique",
        "operation": "unique",
        "input_selector": ["start", "items"],
        "output_variable": "unique_result"
      }
    },
    {
      "id": "op_reverse",
      "data": {
        "type": "list-operator",
        "title": "Reverse",
        "operation": "reverse",
        "input_selector": ["op_unique", "unique_result"],
        "output_variable": "reversed"
      }
    },
    {
      "id": "op_first",
      "data": {
        "type": "list-operator",
        "title": "First",
        "operation": "first",
        "input_selector": ["op_reverse", "reversed"],
        "output_variable": "first_val"
      }
    },
    {
      "id": "op_last",
      "data": {
        "type": "list-operator",
        "title": "Last",
        "operation": "last",
        "input_selector": ["op_reverse", "reversed"],
        "output_variable": "last_val"
      }
    },
    {
      "id": "op_length",
      "data": {
        "type": "list-operator",
        "title": "Length",
        "operation": "length",
        "input_selector": ["op_unique", "unique_result"],
        "output_variable": "len"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "first", "value_selector": ["op_first", "first_val"] },
          { "variable": "last", "value_selector": ["op_last", "last_val"] },
          { "variable": "length", "value_selector": ["op_length", "len"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "op_unique" },
    { "source": "op_unique", "target": "op_reverse" },
    { "source": "op_unique", "target": "op_length" },
    { "source": "op_reverse", "target": "op_first" },
    { "source": "op_reverse", "target": "op_last" },
    { "source": "op_first", "target": "end" },
    { "source": "op_last", "target": "end" },
    { "source": "op_length", "target": "end" }
  ]
}
```

执行流程：`[3,1,2,1,3]` → unique → `[3,1,2]` → reverse → `[2,1,3]` → first=2, last=3; length(`[3,1,2]`)=3

**in.json**: `{ "items": [3, 1, 2, 1, 3] }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "first": 2, "last": 3, "length": 3 },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 089_list_op_sort_slice

**目的**：测试 sort（按字段排序）和 slice（切片）操作。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": {
        "type": "start",
        "title": "Start",
        "variables": [
          { "variable": "items", "label": "Items", "type": "array-object", "required": true }
        ]
      }
    },
    {
      "id": "op_sort",
      "data": {
        "type": "list-operator",
        "title": "Sort",
        "operation": "sort",
        "input_selector": ["start", "items"],
        "sort_key": "score",
        "sort_order": "asc",
        "output_variable": "sorted"
      }
    },
    {
      "id": "op_slice",
      "data": {
        "type": "list-operator",
        "title": "Slice",
        "operation": "slice",
        "input_selector": ["op_sort", "sorted"],
        "start": 0,
        "end": 2,
        "output_variable": "sliced"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "result", "value_selector": ["op_slice", "sliced"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "op_sort" },
    { "source": "op_sort", "target": "op_slice" },
    { "source": "op_slice", "target": "end" }
  ]
}
```

**in.json**: `{ "items": [{"name":"c","score":3},{"name":"a","score":1},{"name":"b","score":2}] }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "result": [{"name":"a","score":1},{"name":"b","score":2}] },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 090_list_op_filter

**目的**：测试 filter 操作（需要子图），过滤出大于 2 的元素。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": {
        "type": "start",
        "title": "Start",
        "variables": [
          { "variable": "items", "label": "Items", "type": "array-number", "required": true }
        ]
      }
    },
    {
      "id": "op_filter",
      "data": {
        "type": "list-operator",
        "title": "Filter",
        "operation": "filter",
        "input_selector": ["start", "items"],
        "sub_graph": {
          "nodes": [
            { "id": "sg_start", "type": "start", "title": "SG Start", "data": {} },
            {
              "id": "sg_code", "type": "code", "title": "Check",
              "data": {
                "code": "function main(inputs) { return { keep: inputs.item > 2 }; }",
                "language": "javascript",
                "variables": [
                  { "variable": "item", "value_selector": ["__scope__", "item"] }
                ]
              }
            },
            {
              "id": "sg_end", "type": "end", "title": "SG End",
              "data": {
                "outputs": [
                  { "variable": "keep", "value_selector": ["sg_code", "keep"] }
                ]
              }
            }
          ],
          "edges": [
            { "source": "sg_start", "target": "sg_code" },
            { "source": "sg_code", "target": "sg_end" }
          ]
        },
        "output_variable": "filtered"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "result", "value_selector": ["op_filter", "filtered"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "op_filter" },
    { "source": "op_filter", "target": "end" }
  ]
}
```

**in.json**: `{ "items": [1, 2, 3, 4, 5] }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "result": [3, 4, 5] },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 091_list_op_map

**目的**：测试 map 操作（需要子图），每个元素乘以 10。

**workflow.json**: 与 090 结构相同，改动：
- `operation`: `"map"`
- 子图代码: `function main(inputs) { return { value: inputs.item * 10 }; }`
- 子图 end 输出: `{ "variable": "value", "value_selector": ["sg_code", "value"] }`

**in.json**: `{ "items": [1, 2, 3] }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "result": [10, 20, 30] },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 092_list_op_reduce

**目的**：测试 reduce 操作（需要子图），累加求和。

**workflow.json**: 与 090 结构相同，改动：
- `operation`: `"reduce"`
- `initial_value`: `0`
- 子图代码需要读取 `accumulator` 和 `item`：

```javascript
function main(inputs) { return { value: inputs.accumulator + inputs.item }; }
```

子图变量映射：
```json
"variables": [
  { "variable": "item", "value_selector": ["__scope__", "item"] },
  { "variable": "accumulator", "value_selector": ["__scope__", "accumulator"] }
]
```

子图 end 输出: `{ "variable": "value", "value_selector": ["sg_code", "value"] }`

**in.json**: `{ "items": [1, 2, 3, 4] }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "result": 10 },
  "partial_match": false
}
```

**state.json**: `{}`

---

## 第四阶段：Variable Assigner 测试（093-095）

覆盖 `src/nodes/data_transform.rs` VariableAssignerExecutor 和 `src/core/dispatcher.rs` 中的 assigner 写入逻辑（Overwrite/Append/Clear）。

### 093_assigner_overwrite

**目的**：测试 Overwrite 模式，用新值覆盖会话变量。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": {
        "type": "start",
        "title": "Start",
        "variables": [
          { "variable": "input_val", "label": "Val", "type": "string", "required": true }
        ]
      }
    },
    {
      "id": "assign1",
      "data": {
        "type": "assigner",
        "title": "Assign Overwrite",
        "assigned_variable_selector": ["conversation", "stored"],
        "input_variable_selector": ["start", "input_val"],
        "write_mode": "overwrite"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "stored", "value_selector": ["conversation", "stored"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "assign1" },
    { "source": "assign1", "target": "end" }
  ],
  "conversation_variables": [
    { "name": "stored", "type": "string", "default": "initial" }
  ]
}
```

**in.json**: `{ "input_val": "new_value" }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "stored": "new_value" },
  "partial_match": false
}
```

**state.json**: `{ "conversation_variables": { "stored": "initial" } }`

---

### 094_assigner_append

**目的**：测试 Append 模式，向数组追加元素。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": { "type": "start", "title": "Start" }
    },
    {
      "id": "assign1",
      "data": {
        "type": "assigner",
        "title": "Append",
        "assigned_variable_selector": ["conversation", "list"],
        "value": 3,
        "write_mode": "append"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "list", "value_selector": ["conversation", "list"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "assign1" },
    { "source": "assign1", "target": "end" }
  ],
  "conversation_variables": [
    { "name": "list", "type": "array-number" }
  ]
}
```

**in.json**: `{}`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "list": [1, 2, 3] },
  "partial_match": false
}
```

**state.json**: `{ "conversation_variables": { "list": [1, 2] } }`

---

### 095_assigner_clear

**目的**：测试 Clear 模式，清空变量为 None。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": { "type": "start", "title": "Start" }
    },
    {
      "id": "assign1",
      "data": {
        "type": "assigner",
        "title": "Clear",
        "assigned_variable_selector": ["conversation", "data"],
        "write_mode": "clear"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "data", "value_selector": ["conversation", "data"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "assign1" },
    { "source": "assign1", "target": "end" }
  ],
  "conversation_variables": [
    { "name": "data", "type": "string" }
  ]
}
```

**in.json**: `{}`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "data": null },
  "partial_match": false
}
```

**state.json**: `{ "conversation_variables": { "data": "important" } }`

---

## 第五阶段：错误策略和其他（096-100）

### 096_error_strategy_fail_branch

**目的**：测试 `ErrorStrategyType::FailBranch`，节点失败后走 "fail-branch" 边。

**关键技术点**：
- `error_strategy` 放在 `NodeData` 层级（`data` 对象内）
- 代码节点需要有 `sourceHandle: "source"` 的成功边和 `sourceHandle: "fail-branch"` 的失败边
- Dispatcher 的 `is_branch_node` 检查是否有 `source_handle != "source"` 的出边

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": { "type": "start", "title": "Start" }
    },
    {
      "id": "risky_code",
      "data": {
        "type": "code",
        "title": "Risky Code",
        "error_strategy": { "type": "fail-branch" },
        "code": "function main(inputs) { throw new Error('intentional failure'); }",
        "language": "javascript"
      }
    },
    {
      "id": "success_tmpl",
      "data": {
        "type": "template-transform",
        "title": "Success Path",
        "template": "success"
      }
    },
    {
      "id": "failure_tmpl",
      "data": {
        "type": "template-transform",
        "title": "Failure Path",
        "template": "caught-error"
      }
    },
    {
      "id": "end_success",
      "data": {
        "type": "end",
        "title": "End Success",
        "outputs": [
          { "variable": "result", "value_selector": ["success_tmpl", "output"] }
        ]
      }
    },
    {
      "id": "end_failure",
      "data": {
        "type": "end",
        "title": "End Failure",
        "outputs": [
          { "variable": "result", "value_selector": ["failure_tmpl", "output"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "risky_code" },
    { "source": "risky_code", "target": "success_tmpl", "sourceHandle": "source" },
    { "source": "risky_code", "target": "failure_tmpl", "sourceHandle": "fail-branch" },
    { "source": "success_tmpl", "target": "end_success" },
    { "source": "failure_tmpl", "target": "end_failure" }
  ]
}
```

**in.json**: `{}`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "result": "caught-error" },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 097_error_strategy_default_value

**目的**：测试 `ErrorStrategyType::DefaultValue`，节点失败后使用默认输出值继续执行。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": { "type": "start", "title": "Start" }
    },
    {
      "id": "failing_code",
      "data": {
        "type": "code",
        "title": "Failing Code",
        "error_strategy": {
          "type": "default-value",
          "default_value": { "result": "fallback_output" }
        },
        "code": "function main(inputs) { throw new Error('boom'); }",
        "language": "javascript"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "result", "value_selector": ["failing_code", "result"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "failing_code" },
    { "source": "failing_code", "target": "end" }
  ]
}
```

**in.json**: `{}`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "result": "fallback_output" },
  "partial_match": false
}
```

**state.json**: `{}`

---

### 098_env_vars_in_workflow

**目的**：测试环境变量通过 `["env", "VAR_NAME"]` 选择器在模板中使用。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": { "type": "start", "title": "Start" }
    },
    {
      "id": "tmpl",
      "data": {
        "type": "template-transform",
        "title": "Use Env Var",
        "template": "API key is {{ api_key }}",
        "variables": [
          { "variable": "api_key", "value_selector": ["env", "API_KEY"] }
        ]
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "result", "value_selector": ["tmpl", "output"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "tmpl" },
    { "source": "tmpl", "target": "end" }
  ]
}
```

**in.json**: `{}`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "result": "API key is sk-test-12345" },
  "partial_match": false
}
```

**state.json**:
```json
{
  "environment_variables": { "API_KEY": "sk-test-12345" }
}
```

---

### 099_conversation_vars

**目的**：测试会话变量的读取和更新（通过 assigner），包含并行 DAG 分支汇聚。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": {
        "type": "start",
        "title": "Start",
        "variables": [
          { "variable": "name", "label": "Name", "type": "string", "required": true }
        ]
      }
    },
    {
      "id": "tmpl_greet",
      "data": {
        "type": "template-transform",
        "title": "Greet",
        "template": "Hello {{ name }}, count={{ count }}",
        "variables": [
          { "variable": "name", "value_selector": ["start", "name"] },
          { "variable": "count", "value_selector": ["conversation", "visit_count"] }
        ]
      }
    },
    {
      "id": "code_inc",
      "data": {
        "type": "code",
        "title": "Increment",
        "code": "function main(inputs) { return { new_count: inputs.count + 1 }; }",
        "language": "javascript",
        "variables": [
          { "variable": "count", "value_selector": ["conversation", "visit_count"] }
        ]
      }
    },
    {
      "id": "assign_count",
      "data": {
        "type": "assigner",
        "title": "Update Count",
        "assigned_variable_selector": ["conversation", "visit_count"],
        "input_variable_selector": ["code_inc", "new_count"],
        "write_mode": "overwrite"
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "greeting", "value_selector": ["tmpl_greet", "output"] },
          { "variable": "new_count", "value_selector": ["conversation", "visit_count"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "tmpl_greet" },
    { "source": "start", "target": "code_inc" },
    { "source": "tmpl_greet", "target": "end" },
    { "source": "code_inc", "target": "assign_count" },
    { "source": "assign_count", "target": "end" }
  ],
  "conversation_variables": [
    { "name": "visit_count", "type": "number", "default": 0 }
  ]
}
```

**in.json**: `{ "name": "Alice" }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": {
    "greeting": "Hello Alice, count=5",
    "new_count": 6
  },
  "partial_match": false
}
```

**state.json**:
```json
{
  "conversation_variables": { "visit_count": 5 }
}
```

---

### 100_diamond_merge

**目的**：测试菱形 DAG 合并模式（两个并行分支汇聚到一个节点），验证 `is_node_ready` 多入边判断逻辑。

**workflow.json**:
```json
{
  "version": "0.1.0",
  "nodes": [
    {
      "id": "start",
      "data": {
        "type": "start",
        "title": "Start",
        "variables": [
          { "variable": "x", "label": "X", "type": "number", "required": true }
        ]
      }
    },
    {
      "id": "branch_a",
      "data": {
        "type": "code",
        "title": "Branch A",
        "code": "function main(inputs) { return { result: inputs.x + 10 }; }",
        "language": "javascript",
        "variables": [
          { "variable": "x", "value_selector": ["start", "x"] }
        ]
      }
    },
    {
      "id": "branch_b",
      "data": {
        "type": "code",
        "title": "Branch B",
        "code": "function main(inputs) { return { result: inputs.x * 2 }; }",
        "language": "javascript",
        "variables": [
          { "variable": "x", "value_selector": ["start", "x"] }
        ]
      }
    },
    {
      "id": "merge_tmpl",
      "data": {
        "type": "template-transform",
        "title": "Merge",
        "template": "A={{ a }}, B={{ b }}",
        "variables": [
          { "variable": "a", "value_selector": ["branch_a", "result"] },
          { "variable": "b", "value_selector": ["branch_b", "result"] }
        ]
      }
    },
    {
      "id": "end",
      "data": {
        "type": "end",
        "title": "End",
        "outputs": [
          { "variable": "merged", "value_selector": ["merge_tmpl", "output"] }
        ]
      }
    }
  ],
  "edges": [
    { "source": "start", "target": "branch_a" },
    { "source": "start", "target": "branch_b" },
    { "source": "branch_a", "target": "merge_tmpl" },
    { "source": "branch_b", "target": "merge_tmpl" },
    { "source": "merge_tmpl", "target": "end" }
  ]
}
```

**in.json**: `{ "x": 5 }`

**out.json**:
```json
{
  "status": "completed",
  "outputs": { "merged": "A=15, B=10" },
  "partial_match": false
}
```

**state.json**: `{}`

---

## 覆盖率影响一览

| 测试用例 | 覆盖的代码路径 | 关键源文件 |
|---------|--------------|-----------|
| 079-084 | IterationNodeExecutor (sequential/parallel), 3种错误模式, max_iterations | `subgraph_nodes.rs`, `sub_graph_runner.rs` |
| 085-087 | LoopNodeExecutor, 循环条件评估, 类型不匹配处理, max_iterations | `subgraph_nodes.rs` |
| 088-092 | ListOperatorNodeExecutor 12种操作中的10种 | `subgraph_nodes.rs` |
| 093-095 | VariableAssignerExecutor, VariablePool::append/clear, dispatcher assigner 逻辑 | `data_transform.rs`, `variable_pool.rs`, `dispatcher.rs` |
| 096-097 | ErrorStrategyType::FailBranch/DefaultValue | `dispatcher.rs` |
| 098 | 环境变量注入和模板使用 | `scheduler.rs` |
| 099 | 会话变量读写 + assigner 更新 | `scheduler.rs`, `data_transform.rs` |
| 100 | 菱形 DAG 合并, is_node_ready 多入边 | `graph/types.rs` |

## 验证方法

```bash
# 运行所有 e2e 测试
cargo test --test integration_tests

# 查看覆盖率
cargo llvm-cov --test integration_tests

# 单独运行某个测试
cargo test --test integration_tests case_079_iteration_sequential
```
