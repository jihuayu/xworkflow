# 流式处理管线设计

## 1. 问题分析

### 1.1 当前流式架构

xworkflow 已有完整的流式基础设施（`src/core/variable_pool.rs:331-700`）：

```
SegmentStream (读端) ←── channel ──→ StreamWriter (写端)

StreamWriter::send(chunk)     → 追加 chunk 到 chunks 数组
StreamWriter::end(final_val)  → 标记完成，设置 final_value
StreamReader::next()          → 从 cursor 位置读取下一个 chunk/end 事件
```

**现有流式节点**：
- **Answer 节点**（`src/nodes/control_flow.rs:126-252`）：检测上游 streaming 变量，创建输出 stream，逐 chunk 渲染模板
- **Code 节点**（`src/nodes/data_transform.rs:1103-1277`）：支持 streaming JS callback（on_chunk/on_end）

### 1.2 当前的限制

#### 问题 1：非流式节点阻塞等待

大部分节点使用 `pool.get_resolved()` 等待流完成后才开始处理：

```rust
// End 节点
let val = variable_pool.get_resolved(&sel).await;  // ← 等待流完成
// Template 节点（非 Answer）
let val = variable_pool.get(&m.value_selector);
match val {
    Segment::Stream(stream) => {
        // 等待流完成，或取快照
        let final_val = stream.collect().await?;
    }
}
```

这意味着 LLM 输出完成**所有** token 后，下游节点才开始处理。

#### 问题 2：中间节点无法透传流

如果 LLM → Template Transform → Answer 的链路中，Template 节点不支持流式输入/输出，整个管线被打断：

```
LLM (streaming) → [阻塞等待] → Template (batch) → [阻塞等待] → Answer (streaming)
                   ^^^^^^^^                        ^^^^^^^^
                   首 token 延迟在此累积
```

理想情况应该是：

```
LLM (streaming) → Template (streaming) → Answer (streaming)
   chunk 1 ────→ transform ────────────→ render → 用户看到
   chunk 2 ────→ transform ────────────→ render → 用户看到
```

#### 问题 3：流式 chunk 的 pool 存储开销

每个 stream chunk 被存储在 `SegmentStream` 的 chunks 数组中：

```rust
struct StreamState {
    chunks: Vec<Segment>,        // 所有 chunk 的历史记录
    final_value: Option<Segment>,
    status: StreamStatus,
}
```

对于长 LLM 输出（几千 token），chunks 数组持续增长。如果多个下游节点读取同一 stream，所有 chunk 都保留在内存中。

---

## 2. 设计方案

### 2.1 核心思路

设计一个 **流式处理管线（Streaming Pipeline）** 机制，允许支持流式处理的节点以 chunk-by-chunk 的方式处理上游 stream，并向下游输出新的 stream，形成零等待的管线。

### 2.2 流式节点分类

| 类别 | 描述 | 节点类型 |
|------|------|---------|
| **Source** | 产生流 | LLM、流式 HTTP |
| **Transform** | 流入 → 处理 → 流出 | Template Transform、Code（streaming） |
| **Sink** | 消费流，输出最终结果 | Answer、End |
| **Blocker** | 必须等待完整输入 | If-Else（需完整值判断条件）、Variable Aggregator |

### 2.3 StreamTransformer Trait

新增 trait，标记节点支持流式处理：

```rust
/// 支持逐 chunk 处理的节点实现此 trait
#[async_trait]
pub trait StreamTransformer: Send + Sync {
    /// 处理单个输入 chunk，返回 0 或多个输出 chunk
    async fn transform_chunk(
        &self,
        chunk: &Segment,
        state: &mut TransformState,
    ) -> Result<Vec<Segment>, NodeError>;

    /// 输入流结束时调用，返回最终输出
    async fn transform_end(
        &self,
        final_value: &Segment,
        state: &mut TransformState,
    ) -> Result<Segment, NodeError>;

    /// 初始化转换状态
    fn init_state(&self, config: &Value) -> TransformState;
}

/// 转换过程的中间状态
pub struct TransformState {
    /// 累积的文本（用于模板渲染等）
    pub accumulated_text: String,
    /// 自定义状态（节点特定）
    pub custom: HashMap<String, Value>,
}
```

### 2.4 管线自动构建

在 dispatcher 执行节点前，检测是否可以建立流式管线：

```rust
fn detect_streaming_pipeline(
    &self,
    node_id: &str,
    pool: &VariablePool,
    executor: &dyn NodeExecutor,
) -> Option<StreamingPipeline> {
    // 1. 检查节点输入中是否有 Segment::Stream
    // 2. 检查节点 executor 是否实现 StreamTransformer
    // 3. 如果都满足，建立管线
    // 4. 否则走传统路径（等待流完成）
}
```

#### 管线执行流程

```
检测到流式管线:
  LLM_stream → TemplateTransform (implements StreamTransformer) → Answer

执行:
  1. 创建输出 stream channel: (output_stream, output_writer)
  2. 将 output_stream 写入 pool
  3. spawn 管线任务:
     let mut reader = input_stream.reader();
     let mut state = transformer.init_state(config);
     loop {
         match reader.next().await {
             StreamEvent::Chunk(chunk) => {
                 let outputs = transformer.transform_chunk(&chunk, &mut state).await?;
                 for out in outputs {
                     output_writer.send(out).await;
                 }
             }
             StreamEvent::End(final_val) => {
                 let final_out = transformer.transform_end(&final_val, &mut state).await?;
                 output_writer.end(final_out).await;
                 break;
             }
             StreamEvent::Error(e) => {
                 output_writer.error(e).await;
                 break;
             }
         }
     }
  4. 节点返回 NodeRunResult，outputs 包含 output_stream
```

### 2.5 Template Transform 流式化

当前 Template Transform 等待所有输入就绪后一次性渲染。流式化设计：

**增量渲染策略**：

对于模板 `"Summary: {{ text }}"`，当 `text` 是流式输入时：

```
chunk 1: "Hello"     → 输出 chunk: "Hello"
chunk 2: " world"    → 输出 chunk: " world"
end:     "Hello world" → 输出 final: "Summary: Hello world"

或者更精确的增量渲染:
chunk 1: "Hello"     → 输出 chunk: "Summary: Hello"   (第一次含前缀)
chunk 2: " world"    → 输出 chunk: " world"            (后续只输出增量)
end:     "Hello world" → 无额外输出 (已全部发送)
```

**限制**：
- 仅适用于单个流式变量的简单模板
- 多流式变量或复杂 Jinja2 表达式（filter、条件）无法增量渲染
- 退回批量模式：等待所有流完成后一次性渲染

```rust
impl StreamTransformer for TemplateTransformExecutor {
    async fn transform_chunk(
        &self,
        chunk: &Segment,
        state: &mut TransformState,
    ) -> Result<Vec<Segment>, NodeError> {
        // 简单模板：直接透传 chunk
        // 复杂模板：累积到 state，不输出
        if state.is_simple_passthrough {
            Ok(vec![chunk.clone()])
        } else {
            state.accumulated_text.push_str(&chunk.to_display_string());
            Ok(vec![]) // 暂不输出
        }
    }

    async fn transform_end(
        &self,
        final_value: &Segment,
        state: &mut TransformState,
    ) -> Result<Segment, NodeError> {
        // 使用完整值渲染模板
        let rendered = render_template(state.template, final_value)?;
        Ok(Segment::String(rendered))
    }
}
```

### 2.6 背压机制

当下游消费慢于上游生产时，需要背压防止内存无限增长：

```rust
pub struct StreamLimitsExtended {
    /// 现有: 最大 chunk 数
    pub max_chunks: usize,
    /// 新增: 背压缓冲区大小
    pub backpressure_buffer: usize,    // 默认: 64
    /// 新增: 背压等待超时
    pub backpressure_timeout: Duration, // 默认: 30s
}
```

**实现**：使用 bounded channel 替代当前的 unbounded Vec：

当前：
```rust
struct StreamState {
    chunks: Vec<Segment>,  // 无限增长
}
```

改为：
```rust
struct StreamState {
    /// 有界缓冲区：生产者在缓冲区满时等待
    buffer: BoundedBuffer<Segment>,
    /// 已消费的 chunk 可以被回收
    consumed_cursor: usize,
}
```

或更简单地：使用 `tokio::sync::mpsc::channel(buffer_size)` 替代 Vec + Notify：

```rust
pub fn channel_with_backpressure(buffer: usize) -> (SegmentStream, StreamWriter) {
    let (tx, rx) = tokio::sync::mpsc::channel(buffer);
    // tx.send() 在缓冲区满时自动等待（背压）
    // rx.recv() 在缓冲区空时自动等待
}
```

### 2.7 零拷贝 Chunk 传递

当前 chunk 存储为 `Segment`，已通过 `Arc` 共享复合类型。进一步优化：

```rust
/// 流式 chunk 使用 Arc 包裹，多个读者共享同一内存
pub struct StreamChunk {
    pub data: Arc<Segment>,
    pub sequence: u64,
}
```

多个下游节点读取同一 stream 时，chunk 数据零拷贝共享。

### 2.8 与 Answer 节点的集成

Answer 节点已有流式支持（`src/nodes/control_flow.rs:126-252`），但可以优化：

当前流程：
```
1. 正则提取模板变量
2. 分类为 static_values / stream_vars
3. spawn 任务聚合 stream chunks
4. 每收到 chunk → 重新渲染完整模板 → 计算 delta → 输出 delta
```

优化后：
```
1. 如果模板是简单拼接（如 "{{#llm.text#}}"），直接透传 chunk
2. 如果模板包含多个变量，使用增量渲染
3. 避免每次 chunk 都渲染完整模板
```

---

## 3. 接口定义

### 3.1 StreamTransformer Trait

```rust
#[async_trait]
pub trait StreamTransformer: Send + Sync {
    /// 是否支持该节点配置的流式处理
    fn supports_streaming(&self, config: &Value) -> bool;

    /// 处理单个 chunk
    async fn transform_chunk(
        &self,
        chunk: &Segment,
        state: &mut TransformState,
        config: &Value,
    ) -> Result<Vec<Segment>, NodeError>;

    /// 流结束处理
    async fn transform_end(
        &self,
        final_value: &Segment,
        state: &mut TransformState,
        config: &Value,
    ) -> Result<Segment, NodeError>;

    /// 错误处理
    async fn transform_error(
        &self,
        error: &str,
        state: &mut TransformState,
    ) -> Result<(), NodeError> {
        Err(NodeError::StreamError(error.to_string()))
    }
}
```

### 3.2 管线描述

```rust
pub struct StreamingPipeline {
    /// 输入 stream 的 selector
    pub input_selector: Selector,
    /// 转换节点 ID
    pub transformer_node_id: String,
    /// 输出 stream key
    pub output_key: String,
}
```

### 3.3 NodeOutputs 扩展（配合 Value↔Segment 设计）

```rust
pub enum NodeOutputs {
    Sync(HashMap<String, Value>),
    SyncSegment(HashMap<String, Segment>),
    Stream {
        ready: HashMap<String, Value>,
        streams: HashMap<String, SegmentStream>,
    },
    StreamSegment {
        ready: HashMap<String, Segment>,
        streams: HashMap<String, SegmentStream>,
    },
}
```

---

## 4. 迁移策略

### 阶段 1：背压机制
1. 为 SegmentStream 添加 bounded buffer 选项
2. 新增 `channel_with_backpressure()` 工厂方法
3. LLM 节点和 Code 流式节点使用 bounded channel
4. 验证不影响现有行为

### 阶段 2：StreamTransformer Trait
1. 定义 trait
2. 在 dispatcher 中添加管线检测逻辑
3. 当检测到流式管线时，spawn 管线任务而非等待流完成

### 阶段 3：Template Transform 流式化
1. 实现 `StreamTransformer for TemplateTransformExecutor`
2. 简单模板（单变量直接拼接）支持 chunk 透传
3. 复杂模板退回批量模式

### 阶段 4：Answer 节点优化
1. 简化 Answer 节点的流式渲染逻辑
2. 简单模板直接透传 chunk，避免重复渲染
3. 性能测试

### 阶段 5：零拷贝与监控
1. StreamChunk 使用 Arc 共享
2. 添加 stream 监控指标（chunk count、latency、buffer 使用率）

---

## 5. 预期收益

### 首 Token 延迟（TTFT）

| 场景 | 当前 | 优化后 | 提升 |
|------|------|-------|------|
| LLM → Answer（已有流式） | ~100ms | ~100ms | 0%（已优化） |
| LLM → Template → Answer | 2-30s（等待全部 token） | ~100ms（管线化） | 95%+ |
| LLM → Code(streaming) → Answer | 已有流式 | 略有改善 | ~10% |

**最大受益场景**：LLM 与 Answer 之间有中间处理节点的工作流。

### 内存

| 场景 | 当前 | 优化后 |
|------|------|-------|
| 长 LLM 输出（10K token） | chunks 数组 ~1MB | 背压缓冲区 ~64 chunks |
| 多读者共享 stream | 每个读者遍历全部 chunks | Arc 共享 |

### 吞吐量

背压机制防止 OOM，在内存受限环境下维持稳定吞吐。

---

## 6. 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| 流式模板渲染不完整 | 部分模板无法增量渲染 | `supports_streaming()` 检查，自动退回批量模式 |
| 背压导致上游阻塞 | LLM 生产速度被限制 | 合理设置 buffer 大小（64-256） |
| 管线错误传播复杂 | 中间节点异常影响上下游 | transform_error() + 自动取消 |
| 与并行执行的交互 | 管线任务生命周期管理 | 管线任务绑定到节点生命周期 |
| chunk 语义不一致 | 不同 stream source 的 chunk 粒度不同 | 文档明确 chunk 语义，transformer 自行处理 |

---

## 7. 关键文件

| 文件 | 改动 |
|------|------|
| `src/core/variable_pool.rs` | SegmentStream 添加 bounded buffer、Arc chunk |
| `src/core/dispatcher.rs` | 管线检测 + 管线任务 spawn |
| `src/nodes/data_transform.rs` | TemplateTransformExecutor 实现 StreamTransformer |
| `src/nodes/control_flow.rs` | Answer 节点流式优化 |
| `src/nodes/executor.rs` | 可选：StreamTransformer trait 定义 |
| `crates/xworkflow-types/src/lib.rs` | StreamTransformer trait 导出 |
