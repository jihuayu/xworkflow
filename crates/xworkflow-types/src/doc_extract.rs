use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

/// Service key for document extractor providers.
pub const DOC_EXTRACT_PROVIDE_KEY: &str = "doc-extract.provider";

/// Desired output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Text,
    Markdown,
}

/// Document extraction request.
#[derive(Debug, Clone)]
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
    pub options: Value,
}

/// Extraction result.
#[derive(Debug, Clone)]
pub struct ExtractionResult {
    /// Extracted text content.
    pub text: String,
    /// Metadata about the extraction.
    pub metadata: DocumentMetadata,
}

/// Document metadata.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DocumentMetadata {
    pub page_count: Option<u32>,
    pub sheet_count: Option<u32>,
    pub encoding: Option<String>,
    pub extractor_used: String,
}

/// Document extraction errors.
#[derive(Debug, Error)]
pub enum ExtractError {
    /// Unsupported file format.
    #[error("unsupported format: {mime_type}")]
    UnsupportedFormat { mime_type: String, filename: Option<String> },
    /// File too large.
    #[error("file too large: {actual_mb:.2} MB > {max_mb} MB")]
    FileTooLarge { actual_mb: f64, max_mb: u64 },
    /// Extraction failed (format parsing error).
    #[error("extraction failed for {format}: {message}")]
    ExtractionFailed { format: String, message: String },
    /// File download failed.
    #[error("download failed: {0}")]
    DownloadFailed(String),
    /// Encoding error.
    #[error("encoding error: {0}")]
    EncodingError(String),
    /// Password-protected file.
    #[error("password protected")]
    PasswordProtected,
    /// Corrupted file.
    #[error("corrupted file: {0}")]
    CorruptedFile(String),
    /// Extraction timed out.
    #[error("timeout after {seconds} seconds")]
    Timeout { seconds: u64 },
}

/// Document extractor provider interface.
#[async_trait]
pub trait DocumentExtractorProvider: Send + Sync + 'static {
    /// Display name of this provider.
    fn name(&self) -> &str;

    /// List of MIME types this provider can handle.
    fn supported_mime_types(&self) -> Vec<&str>;

    /// List of file extensions this provider can handle (without dot).
    fn supported_extensions(&self) -> Vec<&str>;

    /// Extract text from the given file content.
    async fn extract(&self, request: ExtractionRequest) -> Result<ExtractionResult, ExtractError>;

    /// Priority for MIME type conflict resolution.
    fn priority(&self) -> i32 {
        0
    }
}

/// Wrapper for service registry downcast.
pub struct DocumentExtractorProviderWrapper(pub Arc<dyn DocumentExtractorProvider>);
