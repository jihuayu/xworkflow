# 文档提取器（Document Extractor）节点设计

## Context

xworkflow 的 `NodeType` 枚举中已包含 `DocumentExtractor`，但目前注册的是 `StubExecutor`（`executor.rs:110`）。需要设计一个完整的文档提取器节点，支持从多种格式的文件中提取文本内容，供下游 LLM、模板、条件判断等节点消费。

设计参考了 Dify（最全面的格式支持 + MIME 路由）、Coze（PaddleOCR 插件化 OCR）、n8n（结构化 JSON 输出）三个平台的同类功能，结合 xworkflow 的 Rust 技术栈和现有插件架构。

### 竞品分析摘要

| 特性 | Dify | Coze | n8n |
|------|------|------|-----|
| **专用节点** | 有（Document Extractor） | 无（知识库 + 插件） | 有（Extract From File） |
| **PDF** | pypdfium2（纯文本） | 内置 + PaddleOCR（含扫描件） | 纯文本 |
| **DOCX** | python-docx | 内置 | 不支持（XML workaround） |
| **Excel** | pandas → Markdown 表格 | 内置 | JSON 行对象 |
| **OCR** | 无 | PaddleOCR PP-StructureV3 | 无 |
| **表格提取** | Markdown 表格 | 布局分析 | JSON 行对象 |
| **输出格式** | string / array[string] | 知识库分段 | JSON 对象 |
| **格式覆盖** | 最广（17+ MIME 类别） | 中等（PDF/TXT/MD/DOCX/CSV/XLSX） | 中等（CSV/HTML/JSON/PDF/XLS/XLSX） |

**设计取舍**：
- 采用 Dify 的 MIME 路由 + 宽格式覆盖思路
- 采用 Coze 的可插拔 OCR 架构（不内置 OCR，通过 Provider 插件扩展）
- 输出格式以 LLM 友好的文本/Markdown 为主（非 JSON 行对象）

---

## 1. 功能概述

文档提取器节点接收一个或多个文件引用（`FileSegment`），将文件内容提取为纯文本或 Markdown 格式的字符串，写入输出变量供下游节点使用。

**核心定位**：文件 → 文本的转换桥梁，是 LLM 处理文档类工作流的关键前置节点。

**典型工作流**：
```
Start(file upload) → DocumentExtractor → LLM(summarize) → End
Start(file upload) → DocumentExtractor → TemplateTransform → End
```

---

## 2. 架构设计

### 2.1 整体架构

采用与 Code Node 完全一致的**插件 + Provider** 模式：

```
xworkflow-types (trait 定义)
    └── DocumentExtractorProvider trait    ← 新增
    └── DOC_EXTRACT_PROVIDE_KEY 常量

xworkflow-nodes-docextract (plugin crate)  ← 新增 crate
    └── DocumentExtractPlugin (Plugin trait impl)
    └── DocumentExtractorExecutor (NodeExecutor trait impl)
    └── register/query helpers

xworkflow-docextract-builtin (内置 provider)  ← 新增 crate
    └── BuiltinDocExtractProvider (实现 DocumentExtractorProvider)
    └── 各格式提取器模块
```

**与 Code Node 的架构对照**：

| Code Node（已有） | Document Extractor（新增） |
|---|---|
| `xworkflow-types` 定义 `LanguageProvider` trait | `xworkflow-types` 定义 `DocumentExtractorProvider` trait |
| `xworkflow-nodes-code` 注册 `CodeNodeExecutor` | `xworkflow-nodes-docextract` 注册 `DocumentExtractorExecutor` |
| `xworkflow-sandbox-boa` 提供 JS Provider | `xworkflow-docextract-builtin` 提供内置格式 Provider |
| `SandboxManager` 管理多个 LanguageProvider | `ExtractorRouter` 管理多个 DocumentExtractorProvider |

### 2.2 Provider 发现机制

```
                  PluginContext
                      │
        ┌─────────────┼────────────────┐
        ▼             ▼                ▼
  BuiltinProvider  OcrProvider    CustomProvider
  (PDF,DOCX,CSV)  (Tesseract)   (用户扩展)
        │             │                │
        └─────────────┼────────────────┘
                      ▼
          DocumentExtractorExecutor
           (MIME路由 → Provider选择)
```

节点启动时通过 `query_services(DOC_EXTRACT_PROVIDE_KEY)` 收集所有已注册的 Provider，按 MIME type 建立路由表。

### 2.3 MIME 路由策略

提取请求的分发逻辑（两级路由）：

1. **MIME type 精确匹配**：根据 `FileSegment.mime_type` 查找 Provider
2. **扩展名回退**：如果 MIME type 缺失或无匹配，使用 `FileSegment.filename` 的扩展名通过 `mime_guess` 推断
3. **Provider 优先级**：当多个 Provider 注册同一 MIME type 时，`priority()` 值高的优先（允许用户插件覆盖内置实现）

---

## 3. 支持格式与 Rust 库选型

### 3.1 第一优先级（P0 — 内置支持）

| 格式 | 扩展名 | MIME Type | Rust 库 | 输出格式 |
|------|--------|-----------|---------|---------|
| **纯文本** | .txt | text/plain | 直接读取 + `encoding_rs` + `chardetng` | 原文 |
| **Markdown** | .md | text/markdown | 直接读取 | 原文 |
| **PDF** | .pdf | application/pdf | `pdf-extract` | 纯文本（按页分隔） |
| **DOCX** | .docx | application/vnd.openxmlformats-officedocument.wordprocessingml.document | `zip` + `quick-xml` 解析 | Markdown（段落+表格） |
| **XLSX** | .xlsx | application/vnd.openxmlformats-officedocument.spreadsheetml.sheet | `calamine` | Markdown 表格 |
| **XLS** | .xls | application/vnd.ms-excel | `calamine` | Markdown 表格 |
| **CSV** | .csv | text/csv | `csv` crate | Markdown 表格 |
| **HTML** | .html | text/html | `html2text` | 纯文本 |
| **JSON** | .json | application/json | `serde_json` | 格式化文本 |
| **YAML** | .yaml/.yml | application/x-yaml | `serde_yaml` | 格式化文本 |
| **XML** | .xml | application/xml | `quick-xml` | 纯文本（提取文本节点） |

### 3.2 第二优先级（P1 — 内置支持，较低频）

| 格式 | 扩展名 | Rust 库 | 说明 |
|------|--------|---------|------|
| **RTF** | .rtf | `rtf-parser` | 富文本提取 |
| **代码文件** | .py/.js/.rs/.go/... | 直接读取 + 编码检测 | 与纯文本相同策略，Markdown 模式保留语言标注 |
| **ODS** | .ods | `calamine` | OpenDocument 表格 |

### 3.3 第三优先级（P2 — 通过外部服务/插件支持）

| 格式 | 扩展名 | 方案 | 说明 |
|------|--------|------|------|
| **DOC** | .doc | 外部 API（Unstructured/LibreOffice） | 二进制格式，无可靠 Rust 库 |
| **PPT/PPTX** | .ppt/.pptx | 外部 API | 同上 |
| **EPUB** | .epub | `zip` + XHTML 解析 | 可内置但优先级低 |
| **扫描 PDF/OCR** | .pdf(图片) | 外部 OCR 服务 | 通过 OcrProvider 插件 |

---

## 4. DSL 配置格式

### 4.1 节点配置示例

```json
{
  "id": "doc_extract_1",
  "data": {
    "type": "document-extractor",
    "title": "提取文档内容",
    "variables": [
      {
        "variable": "file",
        "value_selector": ["start", "uploaded_file"]
      }
    ],
    "output_format": "markdown",
    "extract_options": {
      "pdf": {
        "page_range": null,
        "extract_tables": true
      },
      "spreadsheet": {
        "sheet_index": null,
        "max_rows": 10000,
        "include_header": true
      },
      "encoding": null,
      "max_file_size_mb": 50,
      "fail_fast": false
    }
  }
}
```

### 4.2 配置字段说明

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `variables` | array | 是 | — | 输入变量列表，需引用 `file` 或 `array[file]` 类型变量 |
| `output_format` | string | 否 | `"text"` | 输出格式：`"text"` \| `"markdown"` |
| `extract_options.pdf.page_range` | string\|null | 否 | null（全部） | PDF 页码范围，如 `"1-5"` 或 `"1-3,5,8-10"` |
| `extract_options.pdf.extract_tables` | bool | 否 | true | 是否尝试提取 PDF 中的表格 |
| `extract_options.spreadsheet.sheet_index` | int\|null | 否 | null（全部） | 指定工作表索引（0-based），null 表示提取所有 sheet |
| `extract_options.spreadsheet.max_rows` | int | 否 | 10000 | 最大行数限制，防止超大表格 |
| `extract_options.spreadsheet.include_header` | bool | 否 | true | 是否包含表头行 |
| `extract_options.encoding` | string\|null | 否 | null（自动检测） | 强制指定编码（如 `"gbk"`、`"shift-jis"`） |
| `extract_options.max_file_size_mb` | int | 否 | 50 | 单文件最大大小（MB） |
| `extract_options.fail_fast` | bool | 否 | false | 多文件时，单个失败是否终止整个节点 |

---

## 5. 输入输出设计

### 5.1 输入

| 输入类型 | 变量类型 | 说明 |
|---------|---------|------|
| 单文件 | `file`（FileSegment Object） | Start 节点的文件上传变量 |
| 多文件 | `array[file]`（ArrayFileSegment） | Start 节点的多文件上传变量 |

FileSegment 必须包含以下字段之一用于获取文件内容：
- `url`：远程文件 URL（通过 HTTP 下载）
- `id` + `tenant_id`：内部文件存储引用（通过 RuntimeContext 的 FileStorage 获取）

### 5.2 输出

| 输出变量 | 类型 | 说明 |
|---------|------|------|
| `text` | `string`（单文件）或 `array[string]`（多文件） | 提取的文本内容 |
| `metadata` | `object`（单文件）或 `array[object]`（多文件） | 文件元信息 |

**`metadata` 结构示例**：
```json
{
  "filename": "report.pdf",
  "mime_type": "application/pdf",
  "page_count": 12,
  "file_size": 1048576,
  "encoding": "utf-8",
  "extractor_used": "pdf-extract",
  "error": null
}
```

多文件失败场景中，失败文件的 metadata 会包含 `"error": "错误描述"`。

### 5.3 输出格式策略

**`text` 模式**：纯文本输出，适合通用场景

**`markdown` 模式**：
- 表格数据 → Markdown 表格语法
- DOCX 标题 → Markdown 标题层级（`#`/`##`/`###`）
- DOCX 列表 → Markdown 列表（`-` 无序 / `1.` 有序）
- 代码文件 → Markdown 代码块（带语言标注）
- PDF 分页 → `---` 分隔符
- 多 sheet 表格 → 每个 sheet 带 `### SheetName` 标题

---

## 6. Trait 设计

### 6.1 核心 Trait（位于 `xworkflow-types`）

```rust
// crates/xworkflow-types/src/doc_extract.rs

/// Service key for document extractor providers.
pub const DOC_EXTRACT_PROVIDE_KEY: &str = "doc-extract.provider";

/// Document extraction request.
pub struct ExtractionRequest {
    /// Raw file bytes.
    pub content: Vec<u8>,
    /// Detected or declared MIME type.
    pub mime_type: String,
    /// Original filename (for extension-based fallback).
    pub filename: Option<String>,
    /// Output format preference.
    pub output_format: OutputFormat,
    /// Format-specific options (JSON blob from DSL config).
    pub options: serde_json::Value,
}

/// Desired output format.
pub enum OutputFormat {
    Text,       // 纯文本
    Markdown,   // Markdown 格式
}

/// Extraction result.
pub struct ExtractionResult {
    /// Extracted text content.
    pub text: String,
    /// Metadata about the extraction.
    pub metadata: DocumentMetadata,
}

/// Document metadata.
pub struct DocumentMetadata {
    pub page_count: Option<u32>,
    pub sheet_count: Option<u32>,
    pub encoding: Option<String>,
    pub extractor_used: String,
}

/// Document extractor provider interface.
///
/// Each provider handles one or more MIME types / file formats.
/// Multiple providers can coexist; the executor routes by MIME type.
#[async_trait]
pub trait DocumentExtractorProvider: Send + Sync + 'static {
    /// Display name of this provider.
    fn name(&self) -> &str;

    /// List of MIME types this provider can handle.
    /// Example: ["application/pdf", "text/plain", "text/csv"]
    fn supported_mime_types(&self) -> Vec<&str>;

    /// List of file extensions this provider can handle (without dot).
    /// Used as fallback when MIME type is unavailable.
    /// Example: ["pdf", "txt", "csv"]
    fn supported_extensions(&self) -> Vec<&str>;

    /// Extract text from the given file content.
    async fn extract(
        &self,
        request: ExtractionRequest,
    ) -> Result<ExtractionResult, ExtractError>;

    /// Priority for MIME type conflict resolution.
    /// Higher value = higher priority. Default = 0.
    fn priority(&self) -> i32 { 0 }
}

/// Wrapper for service registry downcast.
pub struct DocumentExtractorProviderWrapper(pub Arc<dyn DocumentExtractorProvider>);
```

### 6.2 错误类型

```rust
/// Document extraction errors.
pub enum ExtractError {
    /// 不支持的文件格式
    UnsupportedFormat { mime_type: String, filename: Option<String> },
    /// 文件过大
    FileTooLarge { actual_mb: f64, max_mb: u64 },
    /// 提取失败（格式解析错误）
    ExtractionFailed { format: String, message: String },
    /// 文件下载失败
    DownloadFailed(String),
    /// 编码错误
    EncodingError(String),
    /// 文件有密码保护
    PasswordProtected,
    /// 文件损坏
    CorruptedFile(String),
    /// 提取超时
    Timeout { seconds: u64 },
}
```

### 6.3 与 LanguageProvider 模式的完整对照

| LanguageProvider（已有） | DocumentExtractorProvider（新增） |
|---|---|
| `LANG_PROVIDE_KEY = "code.lang-provide"` | `DOC_EXTRACT_PROVIDE_KEY = "doc-extract.provider"` |
| `LanguageProviderWrapper(Arc<dyn LanguageProvider>)` | `DocumentExtractorProviderWrapper(Arc<dyn DocumentExtractorProvider>)` |
| `fn language() -> CodeLanguage` | `fn supported_mime_types() -> Vec<&str>` |
| `fn sandbox() -> Arc<dyn CodeSandbox>` | `fn extract(request) -> Result<ExtractionResult, ExtractError>` |
| `fn supports_streaming() -> bool` | `fn priority() -> i32` |
| `SandboxManager` 管理多 Provider | `ExtractorRouter` 管理多 Provider |

---

## 7. 节点执行器设计

### 7.1 DocumentExtractorExecutor

```rust
// crates/xworkflow-nodes-docextract/src/executor.rs

pub struct DocumentExtractorExecutor {
    router: Arc<ExtractorRouter>,
}

/// Routes extraction requests to the appropriate provider.
pub struct ExtractorRouter {
    /// MIME type → sorted providers (by priority desc)
    mime_routes: HashMap<String, Vec<Arc<dyn DocumentExtractorProvider>>>,
    /// Extension → MIME type mapping (fallback)
    ext_to_mime: HashMap<String, String>,
}
```

### 7.2 执行流程

```
execute(node_id, config, variable_pool, context)
  │
  ├── 1. 解析 DocumentExtractorNodeConfig
  │
  ├── 2. 从 variable_pool 读取输入变量
  │     ├── FileSegment (单文件)
  │     └── Vec<FileSegment> (多文件)
  │
  ├── 3. 对每个文件（多文件时并行处理）：
  │     │
  │     ├── 3a. 获取文件内容
  │     │     ├── transfer_method = "url" → HTTP 下载
  │     │     └── transfer_method = "storage" → RuntimeContext 文件存储读取
  │     │
  │     ├── 3b. 文件大小检查 → FileTooLarge
  │     │
  │     ├── 3c. 确定 MIME type
  │     │     ├── 优先：FileSegment.mime_type
  │     │     ├── 回退：filename 扩展名 → mime_guess
  │     │     └── 兜底：infer crate 魔数检测
  │     │
  │     ├── 3d. 路由到 Provider
  │     │     └── ExtractorRouter.route(mime_type) → Provider
  │     │
  │     └── 3e. 调用 provider.extract(request)
  │           └── tokio::time::timeout 包装
  │
  └── 4. 构建 NodeRunResult
        ├── 单文件 → outputs["text"] = Value::String(text)
        ├── 多文件 → outputs["text"] = Value::Array([String, ...])
        └── outputs["metadata"] = 元信息 JSON
```

### 7.3 文件获取抽象

```rust
/// File content resolver — abstracts file acquisition.
#[async_trait]
trait FileContentResolver: Send + Sync {
    async fn resolve(
        &self,
        file: &FileSegment,
        context: &RuntimeContext,
    ) -> Result<Vec<u8>, ExtractError>;
}

/// HTTP-based file resolver (for url transfer method).
struct HttpFileResolver {
    client: reqwest::Client,
    max_size_bytes: u64,
    timeout: Duration,
}

/// Storage-based file resolver (for internal file references).
struct StorageFileResolver;
```

---

## 8. 各格式提取器实现方案

### 8.1 纯文本类（TXT / Markdown / 代码文件）

```
输入 bytes → chardetng 检测编码 → encoding_rs 解码 → 输出 String
```

**编码检测回退链**：
1. 用户指定编码（`extract_options.encoding`）
2. `chardetng` 自动检测（置信度阈值）
3. UTF-8 尝试（`String::from_utf8`）
4. Latin-1（ISO-8859-1）兜底（不会失败）

**代码文件 Markdown 模式处理**：

通过扩展名 → 语言映射表确定语言标注，输出为代码块：

````markdown
```rust
fn main() {
    println!("hello");
}
```
````

扩展名映射示例：`.rs` → `rust`、`.py` → `python`、`.js` → `javascript`、`.go` → `go`、`.java` → `java`、`.cpp` → `cpp`

### 8.2 PDF

使用 `pdf-extract` crate：
- 逐页提取文本
- 页间插入分隔符（text 模式：`\n\n`，markdown 模式：`\n\n---\n\n`）
- 支持 `page_range` 配置，解析语法：`"1-5"` 或 `"1-3,5,8-10"`
- 不支持扫描件 OCR（需通过 OCR Provider 插件支持，见第 9 节）
- metadata 返回 `page_count`

### 8.3 DOCX

使用 `zip` + `quick-xml`：
- DOCX 本质是 ZIP 包，核心内容位于 `word/document.xml`
- 解析 XML 提取：
  - 段落（`<w:p>`）→ 文本段落
  - 表格（`<w:tbl>`）→ Markdown 表格
  - 标题样式（`<w:pStyle w:val="Heading1"/>`）→ `#` 层级
  - 列表项 → `- ` 或 `1. `
- text 模式：去除所有格式标记，纯文本输出
- markdown 模式：保留结构语义

### 8.4 XLSX / XLS / ODS / CSV

**表格类统一处理**：
- XLSX/XLS/ODS：使用 `calamine` crate（统一支持三种格式）
- CSV：使用 `csv` crate

**输出格式**（markdown 模式）：

多 sheet 场景，每个 sheet 独立输出：

```markdown
### Sheet1

| 姓名 | 年龄 | 城市 |
|------|------|------|
| 张三 | 28 | 北京 |
| 李四 | 32 | 上海 |

### Sheet2

| 产品 | 价格 |
|------|------|
| A | 100 |
```

**安全限制**：
- `max_rows` 截断保护（默认 10000 行），超出时在末尾添加 `... (truncated, N more rows)`
- `include_header` 控制是否包含表头行
- `sheet_index` 指定单个 sheet 或 null 提取全部
- metadata 返回 `sheet_count`

### 8.5 HTML

使用 `html2text` crate：
- 自动去除 `<script>`、`<style>` 标签
- 保留基本结构（标题、列表、链接文本）
- markdown 模式下尽量保留格式语义（`<h1>` → `#`，`<a>` → `[text](url)` 等）

### 8.6 JSON / YAML / XML

- **JSON**：`serde_json::from_slice()` → `serde_json::to_string_pretty()` 格式化输出
- **YAML**：`serde_yaml::from_slice()` → 重新序列化为格式化 YAML 文本
- **XML**：`quick-xml` 遍历，提取所有文本节点内容，保留缩进层级

---

## 9. OCR 支持（可插拔架构）

OCR **不**作为内置功能，而是通过 Provider 插件扩展：

```rust
/// 示例：外部 OCR Provider（用户自行注册插件）
pub struct OcrDocExtractProvider {
    api_url: String,    // e.g. PaddleOCR / Tesseract HTTP API
    client: reqwest::Client,
}

#[async_trait]
impl DocumentExtractorProvider for OcrDocExtractProvider {
    fn name(&self) -> &str { "ocr-provider" }

    fn supported_mime_types(&self) -> Vec<&str> {
        vec!["application/pdf", "image/png", "image/jpeg"]
    }

    fn priority(&self) -> i32 { 10 }  // 高优先级，覆盖内置 PDF Provider

    async fn extract(&self, request: ExtractionRequest)
        -> Result<ExtractionResult, ExtractError> {
        // 将文件内容 POST 到外部 OCR 服务 API
        // 解析返回的文本结果
    }
}
```

**设计要点**：
- OCR Provider 注册更高的 `priority()`，自动覆盖内置的 text-only PDF Provider
- 用户可选择性启用 OCR（注册 OCR 插件即启用，不注册则走内置纯文本提取）
- 可支持多种 OCR 后端：PaddleOCR、Tesseract HTTP、云端 OCR API（阿里云/腾讯云）等
- 同时可处理 `image/png`、`image/jpeg` 等图片格式的文字识别

---

## 10. 错误处理

### 10.1 新增 ErrorCode

在 `src/error/error_context.rs` 的 `ErrorCode` 枚举中新增：

```rust
// Document Extraction
DocumentUnsupportedFormat,   // 不支持的文件格式
DocumentExtractionFailed,    // 提取失败（内容解析错误）
DocumentFileTooLarge,        // 文件超出大小限制
DocumentPasswordProtected,   // 文件有密码保护
DocumentDownloadFailed,      // 文件下载失败
```

### 10.2 ExtractError → NodeError 转换映射

| ExtractError | ErrorCode | Retryability | 说明 |
|---|---|---|---|
| `UnsupportedFormat` | `DocumentUnsupportedFormat` | NonRetryable | 格式不支持，重试无意义 |
| `FileTooLarge` | `DocumentFileTooLarge` | NonRetryable | 文件太大，重试无意义 |
| `ExtractionFailed` | `DocumentExtractionFailed` | NonRetryable | 解析错误，文件本身有问题 |
| `DownloadFailed` | `DocumentDownloadFailed` | **Retryable** | 网络问题，可重试 |
| `PasswordProtected` | `DocumentPasswordProtected` | NonRetryable | 密码保护，需用户干预 |
| `CorruptedFile` | `DocumentExtractionFailed` | NonRetryable | 文件损坏 |
| `Timeout` | `Timeout` | **Retryable** | 超时，可重试 |
| `EncodingError` | `DocumentExtractionFailed` | NonRetryable | 编码问题 |

### 10.3 多文件部分失败策略

当处理多个文件时，单个文件提取失败的处理方式取决于 `fail_fast` 配置：

- **`fail_fast: false`（默认）**：
  - 失败文件的 `text` 输出为空字符串 `""`
  - 对应 `metadata` 中标记 `"error": "Extraction failed: ..."`
  - 节点整体状态为 `Succeeded`

- **`fail_fast: true`（严格模式）**：
  - 任何文件失败则整个节点立即失败
  - 返回 `NodeRunResult` 状态为 `Failed`

---

## 11. 安全考量

| 安全项 | 措施 | 说明 |
|--------|------|------|
| **文件大小** | `max_file_size_mb` 配置（默认 50MB），超出直接拒绝 | 防止内存耗尽 |
| **提取超时** | 单文件提取超时（默认 60s），`tokio::time::timeout` | 防止恶意文件阻塞 |
| **内存保护** | 表格 `max_rows` 截断；文本输出最大长度限制（默认 10MB） | 防止超大输出 |
| **路径穿越** | 解压 ZIP（DOCX/XLSX）时验证内部路径，拒绝包含 `..` 的条目 | 防止 ZipSlip 攻击 |
| **ZIP 炸弹** | 限制 ZIP 解压后的总大小（解压/压缩比例阈值 100:1） | 防止资源耗尽 |
| **Tenant 隔离** | `FileSegment.tenant_id` 校验，确保只能访问本租户文件 | 多租户安全 |
| **SSRF 防护** | 文件下载 URL 验证：禁止内网地址（127.0.0.1、10.x、172.16-31.x、192.168.x）| 防止 SSRF 攻击 |
| **HTTPS 优先** | 文件下载默认仅允许 HTTPS，HTTP 需显式配置允许 | 传输安全 |

---

## 12. 性能考量

| 方面 | 策略 |
|------|------|
| **文件下载** | 使用 `reqwest` 异步下载，带超时和大小限制的流式读取（边读边检查大小） |
| **大文件 PDF** | 逐页提取文本（不一次加载全部页面到内存） |
| **大文件表格** | `max_rows` 截断，按行流式读取 |
| **多文件并行** | 使用 `futures::stream::iter(...).buffer_unordered(N)` 并发提取（N = 4） |
| **内存释放** | 提取完成后立即 drop 文件 bytes；输出字符串使用 `String::with_capacity` 预分配 |
| **lazy 初始化** | 使用 `register_lazy` 注册 DocumentExtractorExecutor，首次使用时才初始化 |
| **缓存 reqwest Client** | `reqwest::Client` 复用（连接池），不每次创建新实例 |

---

## 13. 插件集成方式

### 13.1 新增 Crate 结构

**`crates/xworkflow-nodes-docextract/`**（插件 + 执行器）：

```
crates/xworkflow-nodes-docextract/
├── Cargo.toml
└── src/
    ├── lib.rs          # DocumentExtractPlugin + create_plugin() + register/query helpers
    ├── executor.rs     # DocumentExtractorExecutor (NodeExecutor impl)
    └── router.rs       # ExtractorRouter (MIME 路由表)
```

**`crates/xworkflow-docextract-builtin/`**（内置格式 Provider 实现）：

```
crates/xworkflow-docextract-builtin/
├── Cargo.toml
└── src/
    ├── lib.rs          # BuiltinDocExtractProvider + register_builtin_provider() helper
    ├── text.rs         # TXT / Markdown / 代码文件提取
    ├── pdf.rs          # PDF 提取
    ├── docx.rs         # DOCX 提取
    ├── spreadsheet.rs  # XLSX / XLS / ODS / CSV 提取
    ├── html.rs         # HTML 提取
    ├── structured.rs   # JSON / YAML / XML 提取
    └── encoding.rs     # 编码检测工具函数
```

### 13.2 Feature Gate

主 crate `Cargo.toml`：
```toml
[features]
builtin-docextract-node = ["dep:xworkflow-nodes-docextract"]
```

`executor.rs` 注册：
```rust
#[cfg(feature = "builtin-docextract-node")]
{
    registry.register_lazy("document-extractor", Box::new(|| {
        Box::new(super::data_transform::DocumentExtractorExecutor::new())
    }));
}
```

### 13.3 Plugin 注册模式

```rust
// crates/xworkflow-nodes-docextract/src/lib.rs
#[async_trait]
impl Plugin for DocumentExtractPlugin {
    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let providers = query_doc_extract_providers(ctx);
        let router = ExtractorRouter::from_providers(&providers);
        ctx.register_node_executor(
            "document-extractor",
            Box::new(DocumentExtractorExecutor::new(Arc::new(router))),
        )?;
        Ok(())
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(DocumentExtractPlugin::new())
}

/// Register a doc extract provider via the generic service registry.
pub fn register_doc_extract_provider(
    ctx: &mut PluginContext,
    provider: Arc<dyn DocumentExtractorProvider>,
) -> Result<(), PluginError> {
    ctx.provide_service(
        DOC_EXTRACT_PROVIDE_KEY,
        Arc::new(DocumentExtractorProviderWrapper(provider)),
    )
}

/// Query doc extract providers from the service registry.
pub fn query_doc_extract_providers(
    ctx: &PluginContext,
) -> Vec<Arc<dyn DocumentExtractorProvider>> {
    ctx.query_services(DOC_EXTRACT_PROVIDE_KEY)
        .iter()
        .filter_map(|service| {
            service.clone()
                .downcast::<DocumentExtractorProviderWrapper>()
                .ok()
                .map(|w| w.0.clone())
        })
        .collect()
}
```

---

## 14. xworkflow-types 模块变更

在 `crates/xworkflow-types/src/lib.rs` 新增模块导出：

```rust
pub mod doc_extract;

pub use doc_extract::{
    DocumentExtractorProvider, DocumentExtractorProviderWrapper,
    ExtractionRequest, ExtractionResult, DocumentMetadata,
    OutputFormat, ExtractError, DOC_EXTRACT_PROVIDE_KEY,
};
```

---

## 15. 关键依赖

`xworkflow-docextract-builtin` 的 `Cargo.toml`：

```toml
[dependencies]
xworkflow-types = { path = "../xworkflow-types" }
# PDF
pdf-extract = "0.7"
# DOCX (ZIP + XML)
zip = "2.0"
quick-xml = "0.36"
# Spreadsheet (XLSX/XLS/ODS)
calamine = "0.26"
# CSV
csv = "1.3"
# HTML
html2text = "0.12"
# JSON/YAML
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
# Encoding detection
encoding_rs = "0.8"
chardetng = "0.1"
# MIME detection
mime_guess = "2"
# Async
async-trait = "0.1"
tokio = { version = "1", features = ["time"] }
# HTTP download
reqwest = { version = "0.12", features = ["stream"] }
# Error handling
thiserror = "2"
```

---

## 16. 验证方案

### 16.1 单元测试

- 每种格式提取器独立测试（准备小型测试文件嵌入测试代码或 fixtures 目录）
- 编码检测测试（UTF-8、GBK、Shift-JIS、Latin-1 等）
- MIME 路由测试（精确匹配、扩展名回退、优先级覆盖）
- 错误场景测试（文件过大、格式不支持、密码保护 PDF、损坏 ZIP）
- 多 sheet 表格测试、max_rows 截断测试
- page_range 解析测试

### 16.2 集成测试

在 `tests/integration/cases/` 下新增测试用例：

```
tests/integration/cases/
├── 050_docextract_txt/             # 纯文本提取
│   ├── workflow.json
│   └── fixtures/sample.txt
├── 051_docextract_pdf/             # PDF 提取
│   ├── workflow.json
│   └── fixtures/sample.pdf
├── 052_docextract_xlsx/            # 表格提取（含多 sheet）
│   ├── workflow.json
│   └── fixtures/sample.xlsx
├── 053_docextract_docx/            # DOCX 提取
│   ├── workflow.json
│   └── fixtures/sample.docx
├── 054_docextract_multi_file/      # 多文件批量提取
│   ├── workflow.json
│   └── fixtures/
├── 055_docextract_markdown_output/ # Markdown 输出格式
│   └── workflow.json
└── 056_docextract_error/           # 错误处理（不支持的格式、文件过大）
    └── workflow.json
```

### 16.3 验证步骤

1. `cargo test -p xworkflow-docextract-builtin` — 各格式提取器单元测试
2. `cargo test -p xworkflow-nodes-docextract` — 插件注册和路由测试
3. `cargo test --test integration` — 端到端集成测试
4. 手动测试：构造包含 DocumentExtractor 节点的 workflow JSON，通过 API 运行验证

---

## 17. 涉及的关键文件

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `crates/xworkflow-types/src/lib.rs` | 修改 | 新增 `pub mod doc_extract` 导出 |
| `crates/xworkflow-types/src/doc_extract.rs` | **新增** | Trait + 类型定义 |
| `crates/xworkflow-nodes-docextract/` | **新增 crate** | Plugin + Executor + Router |
| `crates/xworkflow-docextract-builtin/` | **新增 crate** | 内置格式提取器实现 |
| `src/nodes/executor.rs:110` | 修改 | Feature gate 注册替换 StubExecutor |
| `src/error/error_context.rs:26-65` | 修改 | 新增 5 个 Document 相关 ErrorCode |
| `Cargo.toml`（workspace root） | 修改 | 新增 workspace members + feature |
