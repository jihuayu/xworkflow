mod docx;
mod encoding;
mod html;
mod pdf;
mod spreadsheet;
mod structured;
mod table;
mod text;

use async_trait::async_trait;
use xworkflow_types::{
    DocumentExtractorProvider,
    DocumentMetadata,
    ExtractError,
    ExtractionRequest,
    ExtractionResult,
    OutputFormat,
};

pub struct BuiltinDocExtractProvider;

impl BuiltinDocExtractProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinDocExtractProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DocumentExtractorProvider for BuiltinDocExtractProvider {
    fn name(&self) -> &str {
        "builtin-doc-extract"
    }

    fn supported_mime_types(&self) -> Vec<&str> {
        vec![
            "text/plain",
            "text/markdown",
            "text/csv",
            "text/html",
            "application/pdf",
            "application/rtf",
            "application/json",
            "application/x-yaml",
            "application/yaml",
            "text/yaml",
            "text/x-yaml",
            "application/xml",
            "text/xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.ms-excel",
            "application/vnd.oasis.opendocument.spreadsheet",
        ]
    }

    fn supported_extensions(&self) -> Vec<&str> {
        vec![
            "txt",
            "md",
            "markdown",
            "pdf",
            "docx",
            "xlsx",
            "xls",
            "ods",
            "csv",
            "html",
            "htm",
            "json",
            "yaml",
            "yml",
            "xml",
            "rtf",
            "rs",
            "py",
            "js",
            "ts",
            "go",
            "java",
            "cpp",
            "c",
            "h",
            "cs",
            "rb",
            "php",
            "swift",
            "kt",
            "scala",
            "sql",
            "sh",
            "bash",
            "ps1",
            "toml",
            "ini",
            "cfg",
            "conf",
        ]
    }

    async fn extract(&self, request: ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
        let format = resolve_format(&request)?;
        match format {
            DocFormat::Text => text::extract_text(&request),
            DocFormat::Markdown => text::extract_text(&request),
            DocFormat::Pdf => pdf::extract_pdf(&request),
            DocFormat::Docx => docx::extract_docx(&request),
            DocFormat::Spreadsheet => spreadsheet::extract_spreadsheet(&request),
            DocFormat::Csv => spreadsheet::extract_csv(&request),
            DocFormat::Html => html::extract_html(&request),
            DocFormat::Json => structured::extract_json(&request),
            DocFormat::Yaml => structured::extract_yaml(&request),
            DocFormat::Xml => structured::extract_xml(&request),
            DocFormat::Rtf => text::extract_rtf(&request),
        }
    }

    fn priority(&self) -> i32 {
        0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocFormat {
    Text,
    Markdown,
    Pdf,
    Docx,
    Spreadsheet,
    Csv,
    Html,
    Json,
    Yaml,
    Xml,
    Rtf,
}

fn resolve_format(request: &ExtractionRequest) -> Result<DocFormat, ExtractError> {
    let mime = request.mime_type.to_lowercase();
    let ext = request
        .filename
        .as_deref()
        .and_then(extension_from_filename)
        .map(|s| s.to_lowercase());

    let by_mime = match mime.as_str() {
        "application/pdf" => Some(DocFormat::Pdf),
        "application/rtf" => Some(DocFormat::Rtf),
        "application/json" => Some(DocFormat::Json),
        "application/x-yaml" | "application/yaml" | "text/yaml" | "text/x-yaml" => {
            Some(DocFormat::Yaml)
        }
        "application/xml" | "text/xml" => Some(DocFormat::Xml),
        "text/html" | "application/xhtml+xml" => Some(DocFormat::Html),
        "text/csv" => Some(DocFormat::Csv),
        "text/markdown" => Some(DocFormat::Markdown),
        "text/plain" => Some(DocFormat::Text),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(DocFormat::Docx)
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.ms-excel"
        | "application/vnd.oasis.opendocument.spreadsheet" => Some(DocFormat::Spreadsheet),
        _ if mime.starts_with("text/") => Some(DocFormat::Text),
        _ => None,
    };

    if let Some(format) = by_mime {
        return Ok(format);
    }

    if let Some(ext) = ext.as_deref() {
        let format = match ext {
            "pdf" => DocFormat::Pdf,
            "docx" => DocFormat::Docx,
            "xlsx" | "xls" | "ods" => DocFormat::Spreadsheet,
            "csv" => DocFormat::Csv,
            "html" | "htm" => DocFormat::Html,
            "json" => DocFormat::Json,
            "yaml" | "yml" => DocFormat::Yaml,
            "xml" => DocFormat::Xml,
            "md" | "markdown" => DocFormat::Markdown,
            "rtf" => DocFormat::Rtf,
            _ => DocFormat::Text,
        };
        return Ok(format);
    }

    Err(ExtractError::UnsupportedFormat {
        mime_type: request.mime_type.clone(),
        filename: request.filename.clone(),
    })
}

fn extension_from_filename(filename: &str) -> Option<String> {
    std::path::Path::new(filename)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

#[allow(dead_code)]
fn output_format_hint(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Text => "text",
        OutputFormat::Markdown => "markdown",
    }
}

fn metadata(extractor_used: &str, encoding: Option<String>) -> DocumentMetadata {
    DocumentMetadata {
        page_count: None,
        sheet_count: None,
        encoding,
        extractor_used: extractor_used.to_string(),
    }
}

fn extraction_failed(format: &str, message: impl Into<String>) -> ExtractError {
    ExtractError::ExtractionFailed {
        format: format.to_string(),
        message: message.into(),
    }
}
