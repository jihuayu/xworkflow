# 原生 File 类型设计

## 1. 背景与动机

### 1.1 当前问题

xworkflow 的 Segment 类型系统中，**文件没有独立的变体**。DSL 层已定义 `file` 和 `array[file]` 类型（`SegmentType::File` / `SegmentType::ArrayFile`），但运行时统一退化为 `Segment::Object`：

```
DSL type: "file"  →  SegmentType::File  →  运行时: Segment::Object
                                            ↑ 语义丢失
```

**现状代码路径**（`variable_pool.rs:316-323`）：

```rust
// 写入：FileSegment → JSON → Object
pub fn to_segment(&self) -> Segment {
    let value = serde_json::to_value(self).unwrap_or(Value::Null);  // 序列化
    Segment::from_value(&value)                                     // 再解析
}

// 读取：Object → JSON → FileSegment
pub fn from_segment(seg: &Segment) -> Option<Self> {
    serde_json::from_value(seg.snapshot_to_value()).ok()             // 两次转换
}
```

每次文件变量的读/写都经历 **双重序列化**（FileSegment ↔ Value ↔ Segment），产生约 16 次堆分配。

### 1.2 为什么需要原生 File 类型

File 在 Web 标准中有明确、成熟的定义（W3C File API），是所有 Web 平台开发者熟悉的抽象。作为工作流引擎的一等类型有三个核心理由：

1. **类型安全**：编译期区分 File 和普通 Object，避免运行时 JSON 反序列化失败
2. **API 人体工学**：`Segment::File(f)` 直接拿到结构化的 `FileSegment`，无需 JSON 往返
3. **语义完整性**：DSL 的 `file` 类型有对应的运行时表示，消除 `SegmentType::File` 匹配 `Segment::Object` 的 hack（`variable_pool.rs:763`）

---

## 2. Web 标准参考

### 2.1 W3C File API

[W3C File API](https://www.w3.org/TR/FileAPI/) 定义了浏览器端文件对象的标准接口：

**Blob 接口**（File 的基类）：

| 字段 | 类型 | 是否必填 | 说明 |
|------|------|---------|------|
| `size` | `unsigned long long` | **必填**（只读） | 文件大小（字节） |
| `type` | `DOMString` | **必填**（只读） | MIME 类型，未知时为空字符串 `""` |

**File 接口**（继承 Blob）：

| 字段 | 类型 | 是否必填 | 说明 |
|------|------|---------|------|
| `name` | `DOMString` | **必填**（只读） | 文件名（不含路径） |
| `lastModified` | `long long` | **必填**（只读） | Unix 毫秒时间戳，未指定时默认为当前时间 |
| `size` | `unsigned long long` | **必填**（继承） | 同 Blob |
| `type` | `DOMString` | **必填**（继承） | 同 Blob |

**核心结论**：Web 标准中 File 只有 4 个属性，全部必填：`name`、`size`、`type`、`lastModified`。

### 2.2 HTTP 传输层

| 标准 | 传递的文件元信息 |
|------|--------------|
| **Content-Disposition**（RFC 6266） | `filename` / `filename*`（可选） |
| **multipart/form-data**（RFC 7578） | `filename`（建议提供）、`Content-Type`（可选，默认 `text/plain`） |
| **Content-Length** | 文件大小（传输层，非文件属性） |

**核心结论**：HTTP 层传输的文件元信息是 filename + MIME type，size 在传输层隐含。

### 2.3 Dify 的文件模型

Dify 的 `File` 类（Python Pydantic model）包含以下字段：

| 字段 | 类型 | 是否必填 | 说明 |
|------|------|---------|------|
| `tenant_id` | `str` | **必填** | 多租户隔离键 |
| `type` | `FileType` 枚举 | **必填** | 文件类别：image/document/audio/video/custom |
| `transfer_method` | `FileTransferMethod` 枚举 | **必填** | 来源：remote_url/local_file/tool_file/datasource_file |
| `remote_url` | `str \| None` | 可选 | remote_url 方式的 URL |
| `related_id` | `str \| None` | 可选 | 内部文件存储引用 |
| `filename` | `str \| None` | 可选 | 文件名 |
| `extension` | `str \| None` | 可选 | 扩展名（含点号） |
| `mime_type` | `str \| None` | 可选 | MIME 类型 |
| `size` | `int` | 可选 | 文件大小，`-1` 表示未知 |

**注意**：Dify 将 `filename`、`mime_type`、`size` 全部设为可选，这在实际使用中会导致大量的 null 检查和回退逻辑。

### 2.4 跨标准对比

| 概念 | W3C File API | HTTP | Dify | xworkflow 建议 |
|------|-------------|------|------|--------------|
| **文件名** | `name`（必填） | `filename`（建议） | `filename`（可选） | **必填** |
| **MIME 类型** | `type`（必填，默认 `""`） | `Content-Type`（可选） | `mime_type`（可选） | **必填**（默认 `application/octet-stream`） |
| **文件大小** | `size`（必填） | `Content-Length`（传输层） | `size`（可选，默认 -1） | **必填** |
| **最后修改** | `lastModified`（必填） | 无 | 无 | 可选 |
| **传输方式** | 无 | 隐含 | `transfer_method`（必填） | **必填** |
| **文件类别** | 无 | 无 | `type`（必填） | 可选（可从 MIME 推导） |

---

## 3. 字段设计

### 3.1 设计原则

1. **Web 标准对齐**：核心字段名和语义与 W3C File API 保持一致
2. **必填字段最小化**：只有"文件必然具有"的属性才设为必填
3. **不可为空 = 进入系统时必须已知**：文件上传/创建时这些信息一定可获取
4. **工作流引擎扩展**：在 Web 标准基础上增加内容获取方式（transfer_method + url/id）

### 3.2 必填字段（不可为空）

| 字段 | Rust 类型 | Web 标准对应 | 必填理由 |
|------|----------|-------------|---------|
| `name` | `String` | `File.name` | 每个文件必有名称。上传时 `multipart/form-data` 提供 filename，工具生成的文件由代码命名，内部存储的文件在入库时必须记录文件名。 |
| `size` | `u64` | `Blob.size` | 文件大小始终可知。上传时由 HTTP `Content-Length` 获得，本地文件由 `fs::metadata` 获得，工具生成的文件在写入时已知长度。这是资源限制和安全检查的前提。 |
| `mime_type` | `String` | `Blob.type` | MIME 类型决定了文件的处理方式（路由到哪个 Provider）。未知时使用 `"application/octet-stream"`（RFC 7578 推荐），而非空字符串或 None，避免所有下游消费者都要处理"类型缺失"的分支。 |
| `transfer_method` | `FileTransferMethod` 枚举 | 无（工作流扩展） | 文件内容的获取方式是工作流引擎必须知道的。不同的 transfer_method 决定了用 HTTP 下载、读本地路径还是查内部存储。使用枚举而非字符串，编译期保证合法性。 |

**为什么 `name` 不能是 Option？**

有人可能认为"工具生成的临时文件可能没有名字"。但在工作流场景中：
- 用户上传文件：浏览器提供 `File.name`
- 工具/代码生成文件：生成时必须命名（如 `output.csv`、`result.json`）
- 内部存储文件：存储系统记录了原始文件名
- 完全匿名的文件没有实际意义——下游节点至少需要扩展名来推断格式

如果极端情况确实无法确定文件名，入口处应赋予默认名（如 `"unnamed"`），而非让所有下游处理 None。

**为什么 `size` 是 `u64` 而非 `Option<u64>`？**

- 文件大小是安全检查（`max_file_size_mb`）的前提条件
- 如果允许 size 为空，每次安全检查都需要额外的 None 处理分支
- 实际上，所有文件来源（上传、下载、本地、生成）都能在入口处确定大小
- 使用 `u64` 而非 `i64`：文件大小不会是负数

**为什么 `mime_type` 是 `String` 而非 `Option<String>`？**

Web 标准中 `Blob.type` 未知时为 `""`（空字符串），Dify 设为 `None`。xworkflow 选择**始终填充**，理由：
- Document Extractor 的 MIME 路由要求类型必须存在
- 未知类型使用 `"application/octet-stream"` 是 RFC 7578 的明确建议
- 消除所有下游消费者的 `if let Some(mime) = ...` 分支
- 入口处（scheduler）可通过 `mime_guess` + 文件内容嗅探确保类型总被填充

### 3.3 可选字段

| 字段 | Rust 类型 | Web 标准对应 | 可选理由 |
|------|----------|-------------|---------|
| `extension` | `Option<String>` | 无 | 扩展名可从 `name` 推导（`Path::extension()`），作为缓存字段存在。不含点号，如 `"pdf"`、`"xlsx"`。某些场景（如 MIME 类型推断回退）可加速查找。 |
| `url` | `Option<String>` | 无 | 仅在 `transfer_method == RemoteUrl` 时有值。本地文件和内部存储文件没有 URL。 |
| `id` | `Option<String>` | 无 | 内部存储系统的文件引用 ID。仅在 `transfer_method == InternalStorage` 或 `ToolFile` 时有值。 |
| `last_modified` | `Option<i64>` | `File.lastModified` | Unix 毫秒时间戳。Web 标准中必填（默认为当前时间），但工作流场景中远程 URL 文件不一定能获取此信息，强制默认为当前时间也无语义价值。 |
| `hash` | `Option<String>` | 无 | 内容哈希（如 SHA-256），用于去重和完整性校验。仅在内部存储或显式计算时可用。 |
| `extra` | `Option<serde_json::Map<String, Value>>` | 无 | 可扩展元数据字典，用于存储业务自定义属性或 Provider 特定信息（如 PDF 页数、图片分辨率等），无需修改核心结构即可扩展。 |

### 3.4 不再作为 FileSegment 字段的属性

| 原字段 | 处理方式 | 理由 |
|--------|---------|------|
| `tenant_id` | 移至 `RuntimeContext` | 租户隔离是运行时安全策略的职责，不是文件本身的属性。Web 标准中无此概念。文件在跨租户传递时不应携带租户标记。`SecurityPolicy` / `ResourceGroup` 已有此能力。 |

---

## 4. FileTransferMethod 枚举

### 4.1 当前问题

现有 `transfer_method` 是 `String` 类型，通过字符串比较判断：

```rust
// document_extract.rs:278
if file.transfer_method == "local" || file.transfer_method == "path" { ... }
```

字符串匹配容易拼错、无法穷举、IDE 无法补全。

### 4.2 枚举设计

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileTransferMethod {
    /// 远程 URL，通过 HTTP(S) 下载获取内容。
    /// 对应字段：`url` 必须有值。
    RemoteUrl,

    /// 本地文件系统路径。
    /// 对应字段：`id` 存储文件路径。
    LocalFile,

    /// 工具/节点生成的文件，存储在内部临时存储中。
    /// 对应字段：`id` 存储内部引用 ID。
    ToolFile,

    /// 内部持久化存储（如对象存储、数据库）。
    /// 对应字段：`id` 存储存储键。
    InternalStorage,
}
```

**与 Dify 对照**：

| Dify | xworkflow | 说明 |
|------|-----------|------|
| `remote_url` | `RemoteUrl` | 一致 |
| `local_file` | `LocalFile` | 一致 |
| `tool_file` | `ToolFile` | 一致 |
| `datasource_file` | `InternalStorage` | 重命名，更通用 |

**Serde 兼容**：使用 `#[serde(rename_all = "snake_case")]`，JSON 序列化为 `"remote_url"` / `"local_file"` 等，与 Dify DSL 格式兼容。

---

## 5. FileSegment 结构体定义

### 5.1 新设计

```rust
/// 文件引用，对应 DSL 类型 `file`。
///
/// 对齐 W3C File API 的核心语义：每个文件必有 name、size、type（mime_type）。
/// 在此基础上扩展 transfer_method 用于工作流引擎的内容获取。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSegment {
    // ========== 必填字段 ==========

    /// 文件名（不含路径）。
    /// 对应 W3C File API: `File.name`。
    pub name: String,

    /// 文件大小（字节）。
    /// 对应 W3C File API: `Blob.size`。
    pub size: u64,

    /// MIME 类型。
    /// 对应 W3C File API: `Blob.type`。
    /// 未知时使用 "application/octet-stream"。
    pub mime_type: String,

    /// 文件内容的获取方式。
    pub transfer_method: FileTransferMethod,

    // ========== 可选字段 ==========

    /// 文件扩展名（不含点号，如 "pdf"）。
    /// 可从 name 推导，作为缓存字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,

    /// 远程文件 URL（transfer_method == RemoteUrl 时使用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// 内部存储引用 ID 或本地文件路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// 最后修改时间（Unix 毫秒时间戳）。
    /// 对应 W3C File API: `File.lastModified`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<i64>,

    /// 内容哈希（如 SHA-256 hex string）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,

    /// 可扩展的额外元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Map<String, Value>>,
}
```

### 5.2 与现有字段的映射

| 旧字段（当前） | 新字段 | 变更说明 |
|-------------|--------|---------|
| `filename: Option<String>` | `name: String` | 重命名 + 必填。对齐 Web 标准 `File.name`。 |
| `size: Option<i64>` | `size: u64` | 必填 + 无符号。文件大小不可能为负数。 |
| `mime_type: Option<String>` | `mime_type: String` | 必填。未知时为 `"application/octet-stream"`。 |
| `transfer_method: String` | `transfer_method: FileTransferMethod` | 字符串 → 枚举。 |
| `extension: Option<String>` | `extension: Option<String>` | 不变。 |
| `url: Option<String>` | `url: Option<String>` | 不变。 |
| `id: Option<String>` | `id: Option<String>` | 不变。 |
| `tenant_id: String` | *移除* | 移至 `RuntimeContext` 安全层。 |
| — | `last_modified: Option<i64>` | 新增。对齐 Web 标准。 |
| — | `hash: Option<String>` | 新增。内容完整性校验。 |
| — | `extra: Option<Map>` | 新增。可扩展元数据。 |

### 5.3 便捷构造方法

```rust
impl FileSegment {
    /// 从 URL 创建文件引用（远程文件）。
    /// name 和 mime_type 可从 URL 路径和 Content-Type 推断。
    pub fn from_url(url: String, name: String, mime_type: String, size: u64) -> Self { ... }

    /// 从本地路径创建文件引用。
    /// name 取路径最后一段，size 通过 fs::metadata 获取。
    pub fn from_local_path(path: String, name: String, mime_type: String, size: u64) -> Self { ... }

    /// 从内部存储 ID 创建文件引用。
    pub fn from_storage_id(id: String, name: String, mime_type: String, size: u64) -> Self { ... }

    /// 获取扩展名（优先取缓存，否则从 name 推导）。
    pub fn extension(&self) -> Option<&str> { ... }

    /// 获取文件类别（从 MIME 类型推导）。
    pub fn file_category(&self) -> FileCategory { ... }
}

/// 文件类别（从 MIME type 推导，非存储字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    Image,      // image/*
    Document,   // application/pdf, application/vnd.*, text/*
    Audio,      // audio/*
    Video,      // video/*
    Other,      // 其他
}
```

---

## 6. Segment 类型系统集成

### 6.1 新增变体

```rust
#[derive(Debug, Clone)]
pub enum Segment {
    None,
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Object(Arc<SegmentObject>),
    ArrayString(Arc<Vec<String>>),
    Array(Arc<SegmentArray>),
    Stream(SegmentStream),
    File(Arc<FileSegment>),              // ← 新增
    ArrayFile(Arc<Vec<FileSegment>>),    // ← 新增
}
```

**为什么用 `Arc<FileSegment>` 而非直接 `FileSegment`？**

- `Segment` 通过 `im::HashMap` 存储在 `VariablePool` 中，使用 COW 语义
- `Arc` 包装使 clone 成本为 O(1)（引用计数递增），而非深拷贝所有 String 字段
- 与 `Object(Arc<SegmentObject>)` / `Array(Arc<SegmentArray>)` 保持一致
- `ArrayFile` 使用 `Arc<Vec<FileSegment>>` 而非 `Arc<SegmentArray>`，避免装箱开销

### 6.2 类型转换

**`to_value()`**：

```rust
Segment::File(f) => serde_json::to_value(f.as_ref()).unwrap_or(Value::Null),
Segment::ArrayFile(files) => Value::Array(
    files.iter().map(|f| serde_json::to_value(f).unwrap_or(Value::Null)).collect()
),
```

**`from_value()`** — 不变：

`from_value` 继续将 JSON Object 解析为 `Segment::Object`。这是合理的，因为 JSON 层面无法区分文件对象和普通对象。文件类型的 Segment 通过以下入口创建：
- `segment_from_type()` 在 scheduler 中根据 DSL 类型提示构造 `Segment::File`
- `FileSegment::to_segment()` 直接构造 `Segment::File`
- 节点输出通过 `set_file_output()` 辅助方法写入

**`FileSegment::to_segment()`** — 零成本重写：

```rust
impl FileSegment {
    pub fn to_segment(self) -> Segment {
        Segment::File(Arc::new(self))
    }

    pub fn from_segment(seg: &Segment) -> Option<&FileSegment> {
        match seg {
            Segment::File(f) => Some(f.as_ref()),
            _ => None,
        }
    }
}
```

### 6.3 `segment_from_type()` 扩展

在 `scheduler.rs` 中增加 File 和 ArrayFile 的类型感知构造：

```rust
fn segment_from_type(value: &Value, seg_type: Option<&SegmentType>) -> Segment {
    match seg_type {
        Some(SegmentType::ArrayString) => { /* 现有逻辑 */ },
        Some(SegmentType::File) => {
            // 从 JSON 对象构造 FileSegment
            if let Ok(file) = serde_json::from_value::<FileSegment>(value.clone()) {
                return Segment::File(Arc::new(file));
            }
            Segment::from_value(value)  // 回退
        },
        Some(SegmentType::ArrayFile) => {
            if let Value::Array(items) = value {
                let files: Vec<FileSegment> = items.iter()
                    .filter_map(|v| serde_json::from_value::<FileSegment>(v.clone()).ok())
                    .collect();
                if files.len() == items.len() {
                    return Segment::ArrayFile(Arc::new(files));
                }
            }
            Segment::from_value(value)  // 回退
        },
        _ => Segment::from_value(value),
    }
}
```

### 6.4 `matches_type()` 更新

```rust
impl Segment {
    pub fn matches_type(&self, t: &SegmentType) -> bool {
        match t {
            SegmentType::Any => true,
            SegmentType::File => matches!(self, Segment::File(_)),
            SegmentType::ArrayFile => matches!(self, Segment::ArrayFile(_)),
            // ... 其他不变
        }
    }

    pub fn segment_type(&self) -> SegmentType {
        match self {
            Segment::File(_) => SegmentType::File,
            Segment::ArrayFile(_) => SegmentType::ArrayFile,
            // ... 其他不变
        }
    }
}
```

`File` 和 `Object` 是完全独立的类型，互不匹配。

### 6.5 序列化/反序列化

```rust
// Serialize: File → JSON Object（与普通 Object 序列化结果相同）
impl Serialize for Segment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Segment::File(f) => f.serialize(serializer),
            Segment::ArrayFile(files) => files.serialize(serializer),
            // ... 其他不变
        }
    }
}

// Deserialize: 始终回退到 from_value，产生 Segment::Object
// 需要 segment_from_type 的类型提示才能得到 Segment::File
```

---

## 7. 性能对比

### 7.1 单次文件变量操作

| 操作 | 当前（Object 路径） | 原生 File 变体 | 改进 |
|------|-------------------|---------------|------|
| **写入** | `serde_json::to_value` + `from_value`（~16 次堆分配） | `Arc::new(file)`（1 次堆分配） | **~16x 减少分配** |
| **读取** | `snapshot_to_value` + `serde_json::from_value`（~16 次堆分配） | 模式匹配 + `Arc::clone`（0 次堆分配） | **零分配** |
| **Pool clone** | `Arc::clone`（O(1)） | `Arc::clone`（O(1)） | 无差别 |
| **内存占用** | Arc + HashMap(8 条目) + 8 个 String key + OnceLock 缓存 ≈ 500-800 bytes | Arc + FileSegment 结构体 ≈ 200-300 bytes | **~60% 减少** |

### 7.2 Document Extractor 节点典型场景

| 场景 | 文件数 | 当前开销 | 优化后开销 |
|------|--------|---------|-----------|
| 单文件提取 | 1 | ~32 次堆分配（读+写） | ~1 次堆分配 |
| 10 文件批量 | 10 | ~320 次堆分配 | ~10 次堆分配 |
| Iteration 循环 1000 文件 | 1000 | ~32,000 次堆分配 | ~1,000 次堆分配 |

**注意**：实际瓶颈仍在文件下载（网络 I/O，毫秒级）和内容提取（PDF 解析等，毫秒到秒级）。类型系统优化的收益在微秒级。但在高频循环场景（Iteration 节点处理大量文件）中，累积的分配压力可能影响 GC/allocator 性能。

---

## 8. DSL 格式

```json
{
  "user_inputs": {
    "uploaded_file": {
      "name": "report.pdf",
      "size": 1048576,
      "mime_type": "application/pdf",
      "transfer_method": "remote_url",
      "url": "https://example.com/report.pdf"
    }
  }
}
```

---

## 9. Document Extractor 节点适配

```rust
fn segment_to_files(seg: &Segment) -> Result<Vec<Arc<FileSegment>>, NodeError> {
    match seg {
        Segment::File(f) => Ok(vec![f.clone()]),
        Segment::ArrayFile(files) => Ok(files.iter().map(|f| Arc::new(f.clone())).collect()),
        Segment::None => Ok(Vec::new()),
        _ => Err(NodeError::TypeError("document-extractor expects file or array[file]".to_string())),
    }
}
```

无需 Object/Array 回退路径。文件变量在入口处（`segment_from_type`）已被正确构造为 `Segment::File` / `Segment::ArrayFile`，下游节点只需模式匹配即可。

---

## 10. 影响范围

### 10.1 需要修改的文件

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `src/core/variable_pool.rs` | **重点修改** | Segment 枚举新增 File/ArrayFile 变体；FileSegment 结构体重设计；to_value/from_value/matches_type 等方法更新 |
| `src/scheduler.rs` | 修改 | `segment_from_type` 增加 File/ArrayFile 分支 |
| `src/nodes/document_extract.rs` | 修改 | `segment_to_files` 适配新变体 |
| `src/core/dispatcher.rs` | 检查 | 确认 `set_node_outputs` 是否需要处理 File 类型输出 |
| `src/dsl/schema.rs` | 检查 | SegmentType::File 的使用无需修改 |
| 所有 `match seg { ... }` 的位置 | 修改 | 新增 `Segment::File` / `Segment::ArrayFile` 分支 |

### 10.2 不需要修改的文件

| 文件 | 理由 |
|------|------|
| `xworkflow-types/` | 不涉及 Segment 类型系统 |
| 节点执行器（Start/End/IfElse 等） | 不直接操作文件类型 |
| Plugin 系统 | 不涉及 |

### 10.3 Segment match 穷举影响

新增变体后，所有 `match self { Segment::None => ..., Segment::String(_) => ..., ... }` 都需要补充 `File` 和 `ArrayFile` 分支。这是 Rust 编译器强制的，不会遗漏。

受影响的方法：
- `to_value()` / `into_value()` / `snapshot_to_value()`
- `from_value()` — 不需要改（Object 继续走 Object）
- `as_string()` — File/ArrayFile 返回 None
- `is_none()` — 不影响
- `segment_type()` — 新增映射
- `matches_type()` — 更新 File/ArrayFile 逻辑
- `Serialize` / `Deserialize` impl
- `PartialEq` — File 比较使用 Arc 指针相等或字段比较

---

## 11. 总结

### 11.1 核心变更

1. **FileSegment** 重设计：4 个必填字段（`name`、`size`、`mime_type`、`transfer_method`） + 5 个可选字段
2. **FileTransferMethod** 从字符串升级为枚举
3. **Segment** 新增 `File(Arc<FileSegment>)` 和 `ArrayFile(Arc<Vec<FileSegment>>)` 变体
4. **`tenant_id`** 从文件对象移至运行时安全上下文

### 11.2 收益

| 维度 | 改进 |
|------|------|
| **类型安全** | 编译期区分 File 和 Object，消除运行时反序列化失败 |
| **API 人体工学** | 模式匹配直接获取 FileSegment，无需 JSON 往返 |
| **性能** | 写入减少 ~16x 堆分配，读取零分配，内存减少 ~60% |
| **语义完整性** | DSL `file` 类型有对应的运行时 `Segment::File`，消除类型系统断层 |
| **Web 标准对齐** | 字段名和语义与 W3C File API 一致，降低学习成本 |
