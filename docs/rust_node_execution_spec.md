# Dify 节点执行逻辑详细规格 (Rust 实现参考)

本文档是 `rust_workflow_engine_spec.md` 的补充，详细描述每个节点类型的执行逻辑。

---

## 1. Start 节点

### 配置 (StartNodeData)
```rust
struct StartNodeData {
    variables: Vec<StartVariable>,
}

struct StartVariable {
    variable: String,       // 变量名
    label: String,          // 显示标签
    var_type: VariableType, // string, number, file, array[file] 等
    required: bool,
    max_length: Option<i32>,
    options: Option<Vec<String>>,  // select 类型的选项
}
```

### 执行逻辑
```
fn run(node_data, variable_pool) -> NodeRunResult:
    // Start 节点的输入变量已在引擎初始化时写入 variable_pool
    // run() 方法只需要收集并返回这些变量作为 outputs

    outputs = {}
    for var in node_data.variables:
        value = variable_pool.get([node_id, var.variable])
        outputs[var.variable] = value

    // 系统变量也作为输出
    sys_query = variable_pool.get(["sys", "query"])
    sys_files = variable_pool.get(["sys", "files"])
    outputs["sys.query"] = sys_query
    outputs["sys.files"] = sys_files

    return NodeRunResult {
        status: Succeeded,
        outputs: outputs,
        edge_source_handle: "source",
    }
```

### 要点
- Start 节点本身不做复杂处理
- 用户输入在引擎启动前通过 `mapping_user_inputs_to_variable_pool()` 写入变量池
- 系统变量 (sys.query, sys.files, sys.user_id 等) 也在启动前写入

---

## 2. End 节点

### 配置 (EndNodeData)
```rust
struct EndNodeData {
    outputs: Vec<OutputVariable>,
}

struct OutputVariable {
    variable: String,                // 输出变量名
    value_selector: Vec<String>,     // 变量选择器，如 ["node_llm", "text"]
}
```

### 执行逻辑
```
fn run(node_data, variable_pool) -> NodeRunResult:
    outputs = {}
    inputs = {}

    for output_var in node_data.outputs:
        value = variable_pool.get(output_var.value_selector)
        outputs[output_var.variable] = value.to_object()
        inputs[output_var.variable] = value.to_object()

    return NodeRunResult {
        status: Succeeded,
        inputs: inputs,
        outputs: outputs,
        edge_source_handle: "source",
    }
```

### 要点
- End 节点是 Response 类型，其 outputs 作为工作流最终输出
- 通过 value_selector 从变量池获取上游节点的输出
- 支持多个输出变量

---

## 3. Answer 节点 (聊天模式)

### 配置 (AnswerNodeData)
```rust
struct AnswerNodeData {
    answer: String,  // 回答模板，支持变量引用 {{#node_id.var_name#}}
}
```

### 执行逻辑
```
fn run(node_data, variable_pool) -> Generator<NodeEvent>:
    // 1. 解析模板中的变量引用
    template = node_data.answer
    // 模板格式: "Hello {{#node_abc.name#}}, your result is {{#node_xyz.output#}}"

    // 2. 替换变量引用为实际值
    rendered = render_template(template, variable_pool)

    // 3. 产生流式输出事件
    yield NodeRunStreamChunkEvent {
        chunk: rendered,
        selector: [node_id, "answer"],
        is_final: true,
    }

    // 4. 返回结果
    yield NodeRunSucceededEvent {
        result: NodeRunResult {
            status: Succeeded,
            outputs: { "answer": rendered },
            edge_source_handle: "source",
        }
    }
```

### 模板变量解析规则
```
// 变量引用格式: {{#selector#}}
// selector 格式: node_id.variable_name 或 sys.variable_name
// 例如:
//   {{#node_abc.text#}} → variable_pool.get(["node_abc", "text"])
//   {{#sys.query#}} → variable_pool.get(["sys", "query"])
//   {{#1721234567890.output#}} → variable_pool.get(["1721234567890", "output"])

fn render_template(template: &str, pool: &VariablePool) -> String:
    regex = /\{\{#([^#]+)#\}\}/g
    template.replace_all(regex, |match|:
        selector_str = match.group(1)  // e.g. "node_abc.text"
        parts = selector_str.split(".")  // ["node_abc", "text"]
        value = pool.get(parts)
        return value.to_string() if value else ""
    )
```

### 要点
- Answer 节点是 Response 类型，支持流式输出
- 模板中的变量引用使用 `{{#...#}}` 语法
- 如果引用的变量不存在，替换为空字符串

---

## 4. LLM 节点

### 配置 (LLMNodeData)
```rust
struct LLMNodeData {
    model: ModelConfig,
    prompt_template: Vec<PromptMessage>,
    memory: Option<MemoryConfig>,
    context: Option<ContextConfig>,
    vision: Option<VisionConfig>,
}

struct ModelConfig {
    provider: String,       // "openai", "anthropic" 等
    name: String,           // "gpt-4", "claude-3" 等
    mode: String,           // "chat" | "completion"
    completion_params: CompletionParams,
}

struct CompletionParams {
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_tokens: Option<i32>,
    presence_penalty: Option<f64>,
    frequency_penalty: Option<f64>,
    stop: Option<Vec<String>>,
}

struct PromptMessage {
    role: String,           // "system" | "user" | "assistant"
    text: String,           // 支持变量引用 {{#node_id.var#}}
    edition_type: Option<String>,  // "basic" | "jinja2"
}

struct MemoryConfig {
    role_prefix: Option<RolePrefix>,
    window: Option<MemoryWindow>,
    query_prompt_template: Option<String>,
}

struct ContextConfig {
    enabled: bool,
    variable_selector: Vec<String>,  // 引用知识库检索结果
}

struct VisionConfig {
    enabled: bool,
    variable_selector: Option<Vec<String>>,  // 引用图片文件
    detail: Option<String>,  // "low" | "high" | "auto"
}
```

### 执行逻辑
```
fn run(node_data, variable_pool) -> Generator<NodeEvent>:
    // 1. 构建 prompt 消息列表
    messages = []
    for msg in node_data.prompt_template:
        text = render_template(msg.text, variable_pool)
        // 如果是 jinja2 模式，使用 Jinja2 渲染
        if msg.edition_type == "jinja2":
            text = jinja2_render(msg.text, variable_pool)
        messages.push(Message { role: msg.role, content: text })

    // 2. 注入上下文 (知识库检索结果)
    if node_data.context.enabled:
        context = variable_pool.get(node_data.context.variable_selector)
        // 将 context 插入到 system 消息或 user 消息中
        inject_context(messages, context)

    // 3. 注入记忆 (对话历史)
    if node_data.memory:
        history = get_conversation_history(memory_config)
        inject_memory(messages, history)

    // 4. 处理视觉输入 (图片)
    if node_data.vision.enabled:
        files = variable_pool.get(node_data.vision.variable_selector)
        inject_vision(messages, files, node_data.vision.detail)

    // 5. 调用 LLM API (流式)
    full_text = ""
    usage = LLMUsage::empty()

    for chunk in llm_client.stream_chat(model_config, messages):
        full_text += chunk.delta
        yield NodeRunStreamChunkEvent {
            chunk: chunk.delta,
            selector: [node_id, "text"],
            is_final: false,
        }
        if chunk.usage:
            usage = chunk.usage

    // 6. 发送最终 chunk
    yield NodeRunStreamChunkEvent {
        chunk: "",
        selector: [node_id, "text"],
        is_final: true,
    }

    // 7. 返回结果
    yield NodeRunSucceededEvent {
        result: NodeRunResult {
            status: Succeeded,
            inputs: { "prompt": messages_to_string(messages) },
            outputs: { "text": full_text, "usage": usage },
            metadata: {
                "total_tokens": usage.total_tokens,
                "total_price": usage.total_price,
                "currency": usage.currency,
            },
            llm_usage: usage,
            edge_source_handle: "source",
        }
    }
```

### 要点
- LLM 节点是最复杂的节点之一
- 支持流式输出，每个 token 产生一个 StreamChunk 事件
- Prompt 模板支持变量引用和 Jinja2 两种模式
- 支持注入知识库上下文、对话历史、图片
- 需要对接多种 LLM 提供商的 API (OpenAI, Anthropic, Azure 等)
- Rust 实现需要使用 HTTP 客户端 (reqwest) 调用 LLM API
- 需要处理 SSE 流式响应解析

---

## 5. IfElse 节点

### 配置 (IfElseNodeData)
```rust
struct IfElseNodeData {
    cases: Vec<Case>,
}

struct Case {
    case_id: String,
    logical_operator: LogicalOperator,  // "and" | "or"
    conditions: Vec<Condition>,
}

struct Condition {
    variable_selector: Vec<String>,
    comparison_operator: ComparisonOperator,
    value: Option<ConditionValue>,  // String, Vec<String>, bool
    sub_variable_condition: Option<SubVariableCondition>,
}

enum ComparisonOperator {
    // 字符串/数组操作
    Contains,       // "contains"
    NotContains,    // "not contains"
    StartWith,      // "start with"
    EndWith,        // "end with"
    Is,             // "is"
    IsNot,          // "is not"
    Empty,          // "empty"
    NotEmpty,       // "not empty"
    In,             // "in"
    NotIn,          // "not in"
    AllOf,          // "all of"
    // 数值操作
    Equal,          // "="
    NotEqual,       // "≠"
    GreaterThan,    // ">"
    LessThan,       // "<"
    GreaterOrEqual, // "≥"
    LessOrEqual,    // "≤"
    Null,           // "null"
    NotNull,        // "not null"
    // 文件操作
    Exists,         // "exists"
    NotExists,      // "not exists"
}
```

### IfElse Execution Logic

**Pseudocode:**

```
fn execute_if_else(node_data, variable_pool) -> edge_source_handle:
    for case in node_data.cases:
        result = evaluate_case(case, variable_pool)
        if result:
            return case.case_id   // route to this case's branch

    return "false"  // else branch

fn evaluate_case(case, variable_pool) -> bool:
    if case.logical_operator == "and":
        for condition in case.conditions:
            if !evaluate_condition(condition, variable_pool):
                return false   // short-circuit: stop on first false
        return true
    else:  // "or"
        for condition in case.conditions:
            if evaluate_condition(condition, variable_pool):
                return true    // short-circuit: stop on first true
        return false

fn evaluate_condition(condition, variable_pool) -> bool:
    actual = variable_pool.get(condition.variable_selector)
    expected = condition.value

    match condition.comparison_operator:
        // --- String / Array checks ---
        Contains =>
            if actual is String: actual.contains(expected)
            if actual is Array:  actual.iter().any(|el| el == expected)
        NotContains =>
            !evaluate(Contains, actual, expected)
        StartWith =>
            actual.to_string().starts_with(expected)
        EndWith =>
            actual.to_string().ends_with(expected)

        // --- Exact equality ---
        Is =>
            actual.to_string() == expected.to_string()   // works for string & bool
        IsNot =>
            actual.to_string() != expected.to_string()

        // --- Emptiness ---
        Empty =>
            actual is null
            || actual == ""
            || (actual is Array && actual.len() == 0)
        NotEmpty =>
            !evaluate(Empty, actual, expected)

        // --- Membership ---
        In =>
            // expected is an array; check if actual is an element
            expected.contains(actual)
        NotIn =>
            !expected.contains(actual)
        AllOf =>
            // expected is an array; every element must be in actual (array)
            expected.iter().all(|e| actual.contains(e))

        // --- Numeric comparison (with type coercion) ---
        Equal =>
            to_number(actual) == to_number(expected)
        NotEqual =>
            to_number(actual) != to_number(expected)
        GreaterThan =>
            to_number(actual) > to_number(expected)
        LessThan =>
            to_number(actual) < to_number(expected)
        GreaterOrEqual =>
            to_number(actual) >= to_number(expected)
        LessOrEqual =>
            to_number(actual) <= to_number(expected)

        // --- Null checks ---
        Null =>
            actual is null
        NotNull =>
            actual is not null

        // --- File existence ---
        Exists =>
            actual is File && file_exists(actual)
        NotExists =>
            actual is not File || !file_exists(actual)

fn to_number(value) -> f64:
    if value is Number: return value
    if value is String: return value.parse::<f64>()   // coerce string → number
    panic("cannot coerce to number")
```

**Key behaviours:**
- AND short-circuits on the first `false` condition (remaining conditions are not evaluated).
- OR short-circuits on the first `true` condition.
- `edge_source_handle` returns the matching `case_id`, or the literal string `"false"` for the else branch.

---

## 6. Code 节点

**Config struct:**

```rust
struct CodeNodeData {
    code: String,                       // user-written source code
    language: CodeLanguage,             // python3 | javascript
    variables: Vec<VariableMapping>,    // input variable mappings
    outputs: HashMap<String, OutputSchema>, // expected output schema
    dependencies: Vec<String>,          // extra packages (pip/npm)
}

enum CodeLanguage {
    Python3,
    JavaScript,
}

struct OutputSchema {
    output_type: CodeOutputType,
    max_string_length: Option<usize>,   // default: 1_000_000
    max_number: Option<f64>,
    min_number: Option<f64>,
    max_precision: Option<u32>,
    max_depth: Option<u32>,             // for OBJECT / nested types
    max_array_length: Option<usize>,
}

enum CodeOutputType {
    STRING,
    NUMBER,
    BOOLEAN,
    OBJECT,
    ARRAY_STRING,
    ARRAY_NUMBER,
    ARRAY_OBJECT,
    ARRAY_BOOLEAN,
}
```

**Execution pseudocode:**

```
fn execute_code_node(node_data, variable_pool):
    // 1. Build runner script that wraps user code
    transformed_code = transform_code(node_data.code, node_data.language, node_data.variables)

    // 2. POST to sandbox
    response = http_post(
        url  = "{CODE_EXECUTION_ENDPOINT}/v1/sandbox/run",
        body = {
            "language":       node_data.language,
            "code":           transformed_code,
            "preload":        "",               // optional preload script
            "enable_network": true
        }
    )

    // 3. Parse output between <<RESULT>> markers
    raw_output = extract_between(response.stdout, "<<RESULT>>", "<<RESULT>>")
    outputs = json_parse(raw_output)

    // 4. Validate each output against schema
    for (name, schema) in node_data.outputs:
        validate_output(outputs[name], schema)

    return outputs

fn transform_code(user_code, language, variables):
    // Wraps user code in a runner that:
    //   a) base64-decodes each input variable
    //   b) calls main(**inputs)
    //   c) prints <<RESULT>> + json(result) + <<RESULT>>
    ...
```

---

## 7. TemplateTransform 节点

**Config struct:**

```rust
struct TemplateTransformNodeData {
    template: String,                       // Jinja2 template string
    variables: Vec<VariableMapping>,        // variables available in template
}
```

**Execution pseudocode:**

```
fn execute_template_transform(node_data, variable_pool):
    // Build a Python script that renders the Jinja2 template
    python_script = format!(
        "from jinja2 import Template\n"
        "template = Template({})\n"
        "result = template.render({})\n"
        "print(result)",
        repr(node_data.template),
        build_render_kwargs(node_data.variables, variable_pool)
    )

    // Send to sandbox with language="python3"
    response = http_post(
        url  = "{CODE_EXECUTION_ENDPOINT}/v1/sandbox/run",
        body = { "language": "python3", "code": python_script, "preload": "", "enable_network": false }
    )

    rendered = response.stdout
    if rendered.len() > 400_000:
        return error("Output exceeds max length of 400,000 characters")

    return { "output": rendered }
```

---

## 8. HttpRequest 节点

**Config struct:**

```rust
struct HttpRequestNodeData {
    method: HttpMethod,                     // GET | POST | PUT | DELETE | PATCH | HEAD
    url: String,                            // may contain {{#node_id.var#}} placeholders
    headers: Vec<KeyValuePair>,
    params: Vec<KeyValuePair>,
    body: HttpBody,
    authorization: Authorization,
    timeout: Option<u64>,                   // seconds; default 10, max configurable
}

enum HttpMethod { GET, POST, PUT, DELETE, PATCH, HEAD }

enum HttpBody {
    None,
    FormData(Vec<KeyValuePair>),
    XWwwFormUrlencoded(Vec<KeyValuePair>),
    RawText(String),
    Json(String),
    Binary(FileRef),
}

enum Authorization {
    NoAuth,
    ApiKey { key: String, value: String, position: ApiKeyPosition },  // header | query | body
    BearerToken { token: String },
    BasicAuth { username: String, password: String },
    CustomHeaders(Vec<KeyValuePair>),
}

enum ApiKeyPosition { Header, Query, Body }
```

**Execution pseudocode:**

```
fn execute_http_request(node_data, variable_pool):
    // 1. Substitute variables in url, headers, params, body
    url     = substitute_variables(node_data.url, variable_pool)
    headers = substitute_variables(node_data.headers, variable_pool)
    params  = substitute_variables(node_data.params, variable_pool)
    body    = substitute_variables(node_data.body, variable_pool)

    // 2. Apply authorization
    match node_data.authorization:
        ApiKey { position: Header, .. } => headers.push(key, value)
        ApiKey { position: Query, .. }  => params.push(key, value)
        ApiKey { position: Body, .. }   => body.append(key, value)
        BearerToken { token }           => headers.push("Authorization", "Bearer {token}")
        BasicAuth { username, password } => headers.push("Authorization", "Basic {base64(user:pass)}")
        CustomHeaders(h)                => headers.extend(h)
        NoAuth                          => {}

    // 3. Send request
    timeout = node_data.timeout.unwrap_or(10)  // default 10 seconds
    response = http_client.request(method, url, headers, params, body, timeout)

    return {
        status_code: response.status_code,  // i32
        body:        response.body,          // String
        headers:     response.headers,       // String (serialised)
    }
```

**Variable substitution** replaces `{{#node_id.var#}}` tokens with the resolved value from the variable pool.

---

## 9. Tool 节点

**Config struct:**

```rust
struct ToolNodeData {
    provider_id: String,
    provider_type: String,                  // "builtin" | "plugin" | "api" | "workflow"
    tool_name: String,
    tool_parameters: Vec<ToolParameter>,
}

struct ToolParameter {
    name: String,
    value: VariableSelector,                // or literal value
    parameter_type: String,
}
```

**Execution pseudocode:**

```
fn execute_tool_node(node_data, variable_pool):
    // 1. Resolve parameters from variable pool
    params = {}
    for p in node_data.tool_parameters:
        params[p.name] = variable_pool.get(p.value)

    // 2. Invoke tool through plugin/builtin tool system
    tool = tool_registry.get(node_data.provider_type, node_data.provider_id, node_data.tool_name)
    result = tool.invoke(params)

    // 3. Return structured output
    return {
        text:  result.text,          // String
        json:  result.json,          // serde_json::Value (optional)
        files: result.files,         // Vec<File> (optional)
    }
    // Some tools support streaming; in that case chunks are emitted as events.
```

---

## 10. KnowledgeRetrieval 节点

**Config struct:**

```rust
struct KnowledgeRetrievalNodeData {
    query_variable_selector: VariableSelector,
    dataset_ids: Vec<String>,
    retrieval_mode: RetrievalMode,
    top_k: u32,
    score_threshold: Option<f64>,
    reranking_model: Option<RerankingModel>,
}

enum RetrievalMode {
    SingleDataset,
    MultiDatasetRouter,
    MultiDatasetAll,
}

struct RerankingModel {
    provider: String,
    model: String,
}
```

**Execution pseudocode:**

```
fn execute_knowledge_retrieval(node_data, variable_pool):
    query = variable_pool.get(node_data.query_variable_selector).to_string()

    match node_data.retrieval_mode:
        SingleDataset =>
            // Query each dataset independently, merge results
            segments = dataset_service.retrieve(
                dataset_ids   = node_data.dataset_ids,
                query         = query,
                top_k         = node_data.top_k,
                score_threshold = node_data.score_threshold,
            )

        MultiDatasetRouter =>
            // LLM picks the best dataset, then retrieve from it
            chosen_id = llm_route(query, node_data.dataset_ids)
            segments = dataset_service.retrieve([chosen_id], query, top_k, score_threshold)

        MultiDatasetAll =>
            // Retrieve from all datasets, then rerank
            segments = dataset_service.retrieve_all(node_data.dataset_ids, query, top_k)
            if let Some(reranker) = node_data.reranking_model:
                segments = reranker.rerank(segments, query, top_k)

    context = segments.iter().map(|s| s.content).collect::<Vec<_>>().join("\n")

    return {
        result:  segments,   // Vec<DocumentSegment>
        context: context,    // String – concatenated segment text
    }
```

---

## 11. QuestionClassifier 节点

**Config struct:**

```rust
struct QuestionClassifierNodeData {
    query_variable_selector: VariableSelector,
    classes: Vec<ClassDef>,
    model: ModelConfig,
    instruction: Option<String>,
}

struct ClassDef {
    id: String,
    name: String,
}
```

**Execution pseudocode:**

```
fn execute_question_classifier(node_data, variable_pool):
    query = variable_pool.get(node_data.query_variable_selector).to_string()

    // Build classification prompt
    prompt = format!(
        "{instruction}\n\n"
        "Classify the following query into exactly one of these classes:\n"
        "{classes}\n\n"
        "Query: {query}\n\n"
        "Return ONLY the class id.",
        instruction = node_data.instruction.unwrap_or(""),
        classes     = node_data.classes.iter()
                        .map(|c| format!("- {} (id: {})", c.name, c.id))
                        .join("\n"),
        query       = query,
    )

    response = llm_invoke(node_data.model, prompt)
    class_id = parse_class_id(response, &node_data.classes)

    // edge_source_handle = class_id  (used for branch routing)
    set_edge_source_handle(class_id)

    class_name = node_data.classes.iter().find(|c| c.id == class_id).unwrap().name
    return { "class_name": class_name }
```

---

## 12. ParameterExtractor 节点

**Config struct:**

```rust
struct ParameterExtractorNodeData {
    query_variable_selector: VariableSelector,
    parameters: Vec<ExtractParam>,
    model: ModelConfig,
    instruction: Option<String>,
}

struct ExtractParam {
    name: String,
    param_type: String,         // "string" | "number" | "bool" | "select"
    description: String,
    required: bool,
    options: Option<Vec<String>>,  // for "select" type
}
```

**Execution pseudocode:**

```
fn execute_parameter_extractor(node_data, variable_pool):
    query = variable_pool.get(node_data.query_variable_selector).to_string()

    if model_supports_function_calling(node_data.model):
        // --- Function-calling path ---
        tool_def = build_function_schema(node_data.parameters)
        response = llm_invoke_with_tools(
            model = node_data.model,
            prompt = format!("{instruction}\n\nExtract parameters from: {query}",
                             instruction = node_data.instruction.unwrap_or("")),
            tools = [tool_def],
        )
        extracted = parse_function_call_args(response)
    else:
        // --- Prompt-based path ---
        prompt = build_extraction_prompt(node_data.parameters, query, node_data.instruction)
        response = llm_invoke(node_data.model, prompt)
        extracted = parse_json_from_response(response)

    // Return extracted parameters as key-value pairs
    return extracted   // e.g. { "name": "Alice", "age": 30, "confirmed": true }
```

---

## 13. VariableAggregator 节点

**Config struct:**

```rust
struct VariableAggregatorNodeData {
    variables: Vec<VariableSelector>,       // ordered list of selectors
    output_type: VarType,
}
```

**Execution pseudocode:**

```
fn execute_variable_aggregator(node_data, variable_pool):
    // Return the first non-null value from the selector list.
    // Typically used to merge outputs from different conditional branches.
    for selector in node_data.variables:
        value = variable_pool.get(selector)
        if value is not null:
            return { "output": value }

    return { "output": null }
```

---

## 14. VariableAssigner 节点

**Config struct:**

```rust
struct VariableAssignerNodeData {
    assigned_variable_selector: VariableSelector,   // target
    input_variable_selector: Option<VariableSelector>, // source (read from pool)
    value: Option<Value>,                            // or a literal value
    write_mode: WriteMode,
}

enum WriteMode {
    Overwrite,
    Append,
    Clear,
}
```

**Execution pseudocode:**

```
fn execute_variable_assigner(node_data, variable_pool):
    // Determine source value
    source_value = if let Some(selector) = node_data.input_variable_selector:
        variable_pool.get(selector)
    else:
        node_data.value.unwrap()

    // Write to target
    match node_data.write_mode:
        Overwrite =>
            variable_pool.set(node_data.assigned_variable_selector, source_value)
        Append =>
            existing = variable_pool.get(node_data.assigned_variable_selector)
            if existing is Array:
                existing.push(source_value)
            elif existing is String:
                existing += source_value.to_string()
            variable_pool.set(node_data.assigned_variable_selector, existing)
        Clear =>
            variable_pool.set(node_data.assigned_variable_selector, null)

    return { "output": variable_pool.get(node_data.assigned_variable_selector) }
```

---

## 15. Iteration 节点 (容器)

**Config struct:**

```rust
struct IterationNodeData {
    iterator_selector: VariableSelector,        // must resolve to an array
    output_selector: VariableSelector,          // which variable to collect per iteration
    is_parallel: bool,
    parallel_nums: Option<u32>,                 // max concurrent iterations
    error_handle_mode: IterationErrorMode,
}

enum IterationErrorMode {
    Terminated,         // stop all iterations on first error
    RemoveAbnormal,     // skip failed iterations in output
    ContinueOnError,    // include null/error marker and continue
}
```

**Execution pseudocode:**

```
fn execute_iteration(node_data, variable_pool, sub_graph):
    items = variable_pool.get(node_data.iterator_selector)  // Vec<Value>
    results = Vec::new()

    if node_data.is_parallel:
        max_concurrent = node_data.parallel_nums.unwrap_or(10)
        results = parallel_map(items, max_concurrent, |index, item| {
            child_pool = variable_pool.child_scope()
            child_pool.set("item", item)
            child_pool.set("index", index)
            execute_sub_graph(sub_graph, child_pool)
            return child_pool.get(node_data.output_selector)
        }, node_data.error_handle_mode)
    else:
        for (index, item) in items.iter().enumerate():
            child_pool = variable_pool.child_scope()
            child_pool.set("item", item)
            child_pool.set("index", index)

            result = execute_sub_graph(sub_graph, child_pool)
            match result:
                Ok(_) =>
                    results.push(child_pool.get(node_data.output_selector))
                Err(e) =>
                    match node_data.error_handle_mode:
                        Terminated      => return error(e)
                        RemoveAbnormal  => continue   // skip this element
                        ContinueOnError => results.push(null)

    return results   // Vec<Value>
```

**Sub-graph:** The nodes between the `iteration-start` marker and the iteration node itself form the sub-graph that is executed once per element.

---

## 16. Loop 节点 (容器)

**Config struct:**

```rust
struct LoopNodeData {
    loop_condition: Vec<CaseCondition>,     // same structure as IfElse conditions
    max_iterations: u32,
    break_condition: Option<Vec<CaseCondition>>,
}
```

**Execution pseudocode:**

```
fn execute_loop(node_data, variable_pool, sub_graph):
    iteration_count = 0
    completed_reason = "condition_not_met"
    last_output = null

    loop:
        // 1. Check loop condition (same evaluator as IfElse)
        if !evaluate_case(node_data.loop_condition, variable_pool):
            completed_reason = "condition_not_met"
            break

        // 2. Check max iterations
        if iteration_count >= node_data.max_iterations:
            completed_reason = "max_iterations"
            break

        // 3. Execute sub-graph
        last_output = execute_sub_graph(sub_graph, variable_pool)
        iteration_count += 1

        // 4. Check break condition
        if let Some(break_cond) = node_data.break_condition:
            if evaluate_case(break_cond, variable_pool):
                completed_reason = "break_condition"
                break

    return {
        output:           last_output,
        completed_reason: completed_reason,
        iteration_count:  iteration_count,
    }
```

**Sub-graph:** The nodes between `loop-start` and `loop-end` markers form the loop body.

---

## 17. DocumentExtractor 节点

**Config struct:**

```rust
struct DocumentExtractorNodeData {
    variable_selector: VariableSelector,    // must resolve to a File variable
}
```

**Execution pseudocode:**

```
fn execute_document_extractor(node_data, variable_pool):
    file = variable_pool.get(node_data.variable_selector)   // File reference

    text = match file.extension():
        "pdf"   => pdf_extractor.extract(file)
        "docx"  => docx_extractor.extract(file)
        "txt"   => read_text(file)
        "csv"   => csv_extractor.extract(file)
        "xlsx"  => xlsx_extractor.extract(file)
        "html"  => html_extractor.extract(file)     // strip tags, keep text
        "md"    => read_text(file)                   // markdown kept as-is
        "epub"  => epub_extractor.extract(file)
        _       => return error("Unsupported format: {}", file.extension())

    return { "text": text }
```

**Supported formats:** PDF, DOCX, TXT, CSV, XLSX, HTML, MD, EPUB.

---

## 18. ListOperator 节点

**Config struct:**

```rust
struct ListOperatorNodeData {
    variable_selector: VariableSelector,        // must resolve to an array
    filter: Option<Vec<FilterCondition>>,
    order_by: Option<OrderBy>,
    limit: Option<usize>,
    slice: Option<(usize, usize)>,              // (start, end) range
    deduplicate: bool,
}

struct FilterCondition {
    field: String,
    operator: ComparisonOperator,               // reuses IfElse operators
    value: Value,
}

struct OrderBy {
    field: String,
    direction: SortDirection,                   // Asc | Desc
}
```

**Execution pseudocode:**

```
fn execute_list_operator(node_data, variable_pool):
    items = variable_pool.get(node_data.variable_selector)  // Vec<Value>

    // 1. Filter
    if let Some(filters) = node_data.filter:
        items = items.into_iter().filter(|item| {
            filters.iter().all(|f| evaluate_filter(item, f))
        }).collect()

    // 2. Deduplicate
    if node_data.deduplicate:
        items = items.into_iter().unique().collect()

    // 3. Sort
    if let Some(order) = node_data.order_by:
        items.sort_by(|a, b| {
            let va = a.get(order.field)
            let vb = b.get(order.field)
            match order.direction:
                Asc  => va.cmp(vb)
                Desc => vb.cmp(va)
        })

    // 4. Slice
    if let Some((start, end)) = node_data.slice:
        items = items[start..end.min(items.len())].to_vec()

    // 5. Limit
    if let Some(limit) = node_data.limit:
        items.truncate(limit)

    return { "result": items }
```

---

## 19. HumanInput 节点

**Config struct:**

```rust
struct HumanInputNodeData {
    form_fields: Vec<FormField>,
}

struct FormField {
    name: String,
    field_type: String,         // "text" | "textarea" | "select" | "number" | "file"
    label: String,
    required: bool,
    options: Option<Vec<String>>,
    default_value: Option<Value>,
}
```

**Execution pseudocode:**

```
fn execute_human_input(node_data, variable_pool):
    // 1. Emit pause event with form configuration
    emit(NodeRunPauseRequestedEvent {
        node_id:     current_node_id,
        form_config: node_data.form_fields,
    })

    // 2. Workflow execution is suspended here.
    //    External system presents form to user and collects input.

    // 3. On resume, form data is injected into variable pool
    form_data = resume_payload.form_data   // HashMap<String, Value>

    for (field_name, value) in form_data:
        variable_pool.set([current_node_id, field_name], value)

    // 4. Return user-submitted data
    return form_data
```

**Note:** This node triggers a workflow pause. The workflow engine must persist state and resume when the user submits the form.

---

## 20. Agent 节点

**Config struct:**

```rust
struct AgentNodeData {
    model: ModelConfig,
    tools: Vec<AgentTool>,
    strategy: AgentStrategy,
    max_iterations: u32,                    // safety limit for tool-calling loops
    prompt: String,                         // system / instruction prompt
}

enum AgentStrategy {
    FunctionCall,       // model uses native function/tool calling
    ReAct,              // Reasoning + Acting prompt pattern
}

struct AgentTool {
    provider_id: String,
    provider_type: String,
    tool_name: String,
    parameters: Vec<ToolParameter>,
}
```

**Execution pseudocode:**

```
fn execute_agent(node_data, variable_pool):
    messages = [
        SystemMessage(node_data.prompt),
        UserMessage(variable_pool.get_user_query()),
    ]
    tool_defs = build_tool_definitions(node_data.tools)
    iteration = 0
    collected_files = Vec::new()

    loop:
        if iteration >= node_data.max_iterations:
            break

        // 1. Call LLM with messages + tool definitions
        response = llm_invoke_with_tools(
            model    = node_data.model,
            messages = messages,
            tools    = tool_defs,
            stream   = true,        // supports streaming
        )

        // 2. Check if LLM wants to call a tool
        if response.has_tool_calls():
            for tool_call in response.tool_calls:
                // Execute the tool
                tool_result = invoke_tool(
                    node_data.tools,
                    tool_call.name,
                    tool_call.arguments,
                    variable_pool,
                )
                // Collect any files
                if tool_result.files:
                    collected_files.extend(tool_result.files)

                // Add assistant message + tool result to conversation
                messages.push(AssistantMessage(response))
                messages.push(ToolResultMessage(tool_call.id, tool_result.text))

            iteration += 1
            continue

        // 3. LLM returned final text answer — done
        return {
            text:  response.text,           // String
            files: collected_files,         // Vec<File>
        }

    // Reached max iterations — return last response
    return {
        text:  messages.last().text,
        files: collected_files,
    }
```

**Streaming:** During execution, each LLM chunk and tool invocation result is emitted as a streaming event so the caller can display incremental progress.
