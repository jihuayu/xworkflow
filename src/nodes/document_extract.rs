use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{FileSegment, FileTransferMethod, Segment, VariablePool};
use crate::dsl::schema::{EdgeHandle, NodeOutputs, NodeRunResult, VariableMapping, WorkflowNodeExecutionStatus};
use crate::error::{ErrorCode, ErrorContext, NodeError};
use crate::nodes::executor::NodeExecutor;

use xworkflow_types::{
    DocumentExtractorProvider,
    ExtractError,
    ExtractionRequest,
    OutputFormat,
};

#[cfg(feature = "security")]
use crate::security::network::{validate_url, SecureHttpClientFactory};

pub struct DocumentExtractorExecutor {
    router: Arc<ExtractorRouter>,
}

impl DocumentExtractorExecutor {
    pub fn new(router: Arc<ExtractorRouter>) -> Self {
        Self { router }
    }

    pub fn new_with_providers(providers: Vec<Arc<dyn DocumentExtractorProvider>>) -> Self {
        let router = ExtractorRouter::from_providers(&providers);
        Self {
            router: Arc::new(router),
        }
    }
}

#[async_trait]
impl NodeExecutor for DocumentExtractorExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let variables = config
            .get("variables")
            .and_then(|v| serde_json::from_value::<Vec<VariableMapping>>(v.clone()).ok())
            .unwrap_or_default();

        if variables.is_empty() {
            return Err(NodeError::ConfigError(
                "document-extractor requires variables".to_string(),
            ));
        }

        let output_format = match config.get("output_format").and_then(|v| v.as_str()) {
            Some("markdown") => OutputFormat::Markdown,
            _ => OutputFormat::Text,
        };

        let extract_options = config
            .get("extract_options")
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new()));

        let max_file_size_mb = extract_options
            .get("max_file_size_mb")
            .and_then(|v| v.as_u64())
            .unwrap_or(50);
        let fail_fast = extract_options
            .get("fail_fast")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let timeout_secs = extract_options
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(60);

        let mut files: Vec<FileSegment> = Vec::new();
        let mut array_input = false;

        for mapping in &variables {
            let seg = variable_pool.get_resolved(&mapping.value_selector).await;
            let mut extracted = segment_to_files(&seg)?;
            if extracted.len() > 1 {
                array_input = true;
            }
            files.append(&mut extracted);
        }

        if files.is_empty() {
            return Err(NodeError::InputValidationError(
                "no files provided to document-extractor".to_string(),
            ));
        }

        let max_output_bytes = max_output_limit(context, node_id);
        let max_size_bytes = max_file_size_mb.saturating_mul(1024 * 1024);

        if fail_fast {
            let mut texts = Vec::new();
            let mut metas = Vec::new();
            for file in files {
                let (text, meta) = process_file(
                    &self.router,
                    file,
                    output_format,
                    extract_options.clone(),
                    max_size_bytes,
                    timeout_secs,
                    context,
                )
                .await
                .map_err(map_extract_error)?;

                enforce_output_limit(node_id, max_output_bytes, text.len())?;
                texts.push(text);
                metas.push(meta);
            }
            return Ok(build_outputs(texts, metas, array_input));
        }

        let router = self.router.clone();
        let results: Vec<(String, Value)> = futures::stream::iter(files)
            .map(|file| {
                let router = router.clone();
                let options = extract_options.clone();
                async move {
                    match process_file(
                        &router,
                        file.clone(),
                        output_format,
                        options,
                        max_size_bytes,
                        timeout_secs,
                        context,
                    )
                    .await
                    {
                        Ok((text, meta)) => (text, meta),
                        Err(err) => (String::new(), error_metadata(&file, &err)),
                    }
                }
            })
            .buffer_unordered(4)
            .collect()
            .await;

        let mut texts = Vec::new();
        let mut metas = Vec::new();
        for (text, meta) in results {
            enforce_output_limit(node_id, max_output_bytes, text.len())?;
            texts.push(text);
            metas.push(meta);
        }

        Ok(build_outputs(texts, metas, array_input))
    }
}

fn build_outputs(texts: Vec<String>, metas: Vec<Value>, array_output: bool) -> NodeRunResult {
    let mut outputs = HashMap::new();
    if array_output || texts.len() > 1 {
        outputs.insert(
            "text".to_string(),
            Segment::Array(Arc::new(crate::core::variable_pool::SegmentArray::new(
                texts.into_iter().map(Segment::String).collect(),
            ))),
        );
        outputs.insert(
            "metadata".to_string(),
            Segment::from_value(&Value::Array(metas)),
        );
    } else {
        let text = texts.into_iter().next().unwrap_or_default();
        let meta = metas.into_iter().next().unwrap_or(Value::Null);
        outputs.insert("text".to_string(), Segment::String(text));
        outputs.insert("metadata".to_string(), Segment::from_value(&meta));
    }

    NodeRunResult {
        status: WorkflowNodeExecutionStatus::Succeeded,
        outputs: NodeOutputs::Sync(outputs),
        edge_source_handle: EdgeHandle::Default,
        ..Default::default()
    }
}

fn segment_to_files(seg: &Segment) -> Result<Vec<FileSegment>, NodeError> {
    match seg {
        Segment::File(file) => Ok(vec![file.as_ref().clone()]),
        Segment::ArrayFile(files) => Ok(files.as_ref().clone()),
        Segment::None => Ok(Vec::new()),
        _ => Err(NodeError::TypeError(
            "document-extractor expects file or array[file]".to_string(),
        )),
    }
}

async fn process_file(
    router: &ExtractorRouter,
    file: FileSegment,
    output_format: OutputFormat,
    options: Value,
    max_size_bytes: u64,
    timeout_secs: u64,
    context: &RuntimeContext,
) -> Result<(String, Value), ExtractError> {
    let content = resolve_file_content(&file, max_size_bytes, timeout_secs, context).await?;
    let content_size = content.len() as u64;
    let mime_type = detect_mime_type(&file, &content);

    let provider = router
        .route(
            &mime_type,
            if file.name.is_empty() { None } else { Some(file.name.as_str()) },
        )
        .ok_or_else(|| ExtractError::UnsupportedFormat {
            mime_type: mime_type.clone(),
            filename: if file.name.is_empty() {
                None
            } else {
                Some(file.name.clone())
            },
        })?;

    let request = ExtractionRequest {
        content,
        mime_type: mime_type.clone(),
        filename: if file.name.is_empty() {
            None
        } else {
            Some(file.name.clone())
        },
        output_format,
        options,
    };

    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), provider.extract(request))
        .await
        .map_err(|_| ExtractError::Timeout { seconds: timeout_secs })??;

    let metadata = serde_json::json!({
        "filename": if file.name.is_empty() { Value::Null } else { Value::String(file.name.clone()) },
        "mime_type": mime_type,
        "page_count": result.metadata.page_count,
        "sheet_count": result.metadata.sheet_count,
        "file_size": content_size,
        "encoding": result.metadata.encoding,
        "extractor_used": result.metadata.extractor_used,
        "error": Value::Null,
    });

    Ok((result.text, metadata))
}

async fn resolve_file_content(
    file: &FileSegment,
    max_size_bytes: u64,
    timeout_secs: u64,
    context: &RuntimeContext,
) -> Result<Vec<u8>, ExtractError> {
    if file.size > max_size_bytes {
        return Err(ExtractError::FileTooLarge {
            actual_mb: file.size as f64 / (1024.0 * 1024.0),
            max_mb: max_size_bytes / (1024 * 1024),
        });
    }

    if let Some(url) = &file.url {
        if url.starts_with("file://") {
            return read_local_file(url, max_size_bytes).await;
        }
        return download_http(url, max_size_bytes, timeout_secs, context).await;
    }

    if matches!(file.transfer_method, FileTransferMethod::LocalFile) {
        if let Some(path) = &file.id {
            return read_local_path(path, max_size_bytes).await;
        }
    }

    Err(ExtractError::DownloadFailed(
        "file content unavailable".to_string(),
    ))
}

async fn read_local_file(url: &str, max_size_bytes: u64) -> Result<Vec<u8>, ExtractError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ExtractError::DownloadFailed(e.to_string()))?;
    let path = parsed
        .to_file_path()
        .map_err(|_| ExtractError::DownloadFailed("invalid file url".to_string()))?;
    read_local_path(path.to_string_lossy().as_ref(), max_size_bytes).await
}

async fn read_local_path(path: &str, max_size_bytes: u64) -> Result<Vec<u8>, ExtractError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| ExtractError::DownloadFailed(e.to_string()))?;
    if meta.len() > max_size_bytes {
        return Err(ExtractError::FileTooLarge {
            actual_mb: meta.len() as f64 / (1024.0 * 1024.0),
            max_mb: max_size_bytes / (1024 * 1024),
        });
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| ExtractError::DownloadFailed(e.to_string()))?;
    Ok(bytes)
}

async fn download_http(
    url: &str,
    max_size_bytes: u64,
    timeout_secs: u64,
    context: &RuntimeContext,
) -> Result<Vec<u8>, ExtractError> {
    #[cfg(feature = "security")]
    if let Some(policy) = context.security_policy().and_then(|p| p.network.as_ref()) {
        validate_url(url, policy)
            .await
            .map_err(|e| ExtractError::DownloadFailed(e.to_string()))?;
    }

    let timeout = Duration::from_secs(timeout_secs);

    #[cfg(feature = "security")]
    let client = if let Some(policy) = context.security_policy().and_then(|p| p.network.as_ref()) {
        SecureHttpClientFactory::build(policy, timeout)
            .map_err(|e| ExtractError::DownloadFailed(e.to_string()))?
    } else {
        reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ExtractError::DownloadFailed(e.to_string()))?
    };

    #[cfg(not(feature = "security"))]
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| ExtractError::DownloadFailed(e.to_string()))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| ExtractError::DownloadFailed(e.to_string()))?;

    if let Some(len) = resp.content_length() {
        if len > max_size_bytes {
            return Err(ExtractError::FileTooLarge {
                actual_mb: len as f64 / (1024.0 * 1024.0),
                max_mb: max_size_bytes / (1024 * 1024),
            });
        }
    }

    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ExtractError::DownloadFailed(e.to_string()))?;
        if buf.len() + chunk.len() > max_size_bytes as usize {
            return Err(ExtractError::FileTooLarge {
                actual_mb: (buf.len() + chunk.len()) as f64 / (1024.0 * 1024.0),
                max_mb: max_size_bytes / (1024 * 1024),
            });
        }
        buf.extend_from_slice(&chunk);
    }

    Ok(buf)
}

fn detect_mime_type(file: &FileSegment, content: &[u8]) -> String {
    if !file.mime_type.is_empty() {
        return file.mime_type.clone();
    }

    if !file.name.is_empty() {
        if let Some(mime) = mime_guess::from_path(&file.name).first() {
            return mime.essence_str().to_string();
        }
    }

    if let Some(ext) = file.extension() {
        if let Some(mime) = mime_guess::from_ext(ext).first() {
            return mime.essence_str().to_string();
        }
    }

    if let Some(kind) = infer::get(content) {
        return kind.mime_type().to_string();
    }

    "application/octet-stream".to_string()
}

fn max_output_limit(context: &RuntimeContext, _node_id: &str) -> usize {
    #[cfg(feature = "security")]
    if let Some(policy) = context.security_policy() {
        if let Some(limit) = policy.node_limits.get("document-extractor") {
            return limit.max_output_bytes;
        }
    }
    10 * 1024 * 1024
}

fn enforce_output_limit(
    node_id: &str,
    max_bytes: usize,
    actual_bytes: usize,
) -> Result<(), NodeError> {
    if actual_bytes > max_bytes {
        return Err(NodeError::OutputTooLarge {
            node_id: node_id.to_string(),
            max: max_bytes,
            actual: actual_bytes,
        });
    }
    Ok(())
}

fn error_metadata(file: &FileSegment, err: &ExtractError) -> Value {
    serde_json::json!({
        "filename": if file.name.is_empty() { Value::Null } else { Value::String(file.name.clone()) },
        "mime_type": file.mime_type,
        "page_count": Value::Null,
        "sheet_count": Value::Null,
        "file_size": file.size,
        "encoding": Value::Null,
        "extractor_used": "",
        "error": err.to_string(),
    })
}

fn map_extract_error(err: ExtractError) -> NodeError {
    let message = err.to_string();
    let context = match err {
        ExtractError::UnsupportedFormat { .. } => {
            ErrorContext::non_retryable(ErrorCode::DocumentUnsupportedFormat, message.clone())
        }
        ExtractError::FileTooLarge { .. } => {
            ErrorContext::non_retryable(ErrorCode::DocumentFileTooLarge, message.clone())
        }
        ExtractError::ExtractionFailed { .. } => {
            ErrorContext::non_retryable(ErrorCode::DocumentExtractionFailed, message.clone())
        }
        ExtractError::DownloadFailed(_) => {
            ErrorContext::retryable(ErrorCode::DocumentDownloadFailed, message.clone())
        }
        ExtractError::EncodingError(_) => {
            ErrorContext::non_retryable(ErrorCode::DocumentExtractionFailed, message.clone())
        }
        ExtractError::PasswordProtected => {
            ErrorContext::non_retryable(ErrorCode::DocumentPasswordProtected, message.clone())
        }
        ExtractError::CorruptedFile(_) => {
            ErrorContext::non_retryable(ErrorCode::DocumentExtractionFailed, message.clone())
        }
        ExtractError::Timeout { .. } => {
            ErrorContext::retryable(ErrorCode::Timeout, message.clone())
        }
    };

    NodeError::ExecutionError(message).with_context(context)
}

pub struct ExtractorRouter {
    mime_routes: HashMap<String, Vec<Arc<dyn DocumentExtractorProvider>>>,
    ext_routes: HashMap<String, Vec<Arc<dyn DocumentExtractorProvider>>>,
}

impl ExtractorRouter {
    pub fn from_providers(providers: &[Arc<dyn DocumentExtractorProvider>]) -> Self {
        let mut mime_routes: HashMap<String, Vec<Arc<dyn DocumentExtractorProvider>>> = HashMap::new();
        let mut ext_routes: HashMap<String, Vec<Arc<dyn DocumentExtractorProvider>>> = HashMap::new();

        for provider in providers {
            for mime in provider.supported_mime_types() {
                let key = mime.to_lowercase();
                mime_routes.entry(key).or_default().push(provider.clone());
            }
            for ext in provider.supported_extensions() {
                let key = ext.to_lowercase();
                ext_routes.entry(key).or_default().push(provider.clone());
            }
        }

        for providers in mime_routes.values_mut() {
            providers.sort_by(|a, b| b.priority().cmp(&a.priority()).then_with(|| a.name().cmp(b.name())));
        }
        for providers in ext_routes.values_mut() {
            providers.sort_by(|a, b| b.priority().cmp(&a.priority()).then_with(|| a.name().cmp(b.name())));
        }

        Self {
            mime_routes,
            ext_routes,
        }
    }

    pub fn route(
        &self,
        mime_type: &str,
        filename: Option<&str>,
    ) -> Option<Arc<dyn DocumentExtractorProvider>> {
        let mime_key = mime_type.to_lowercase();
        if let Some(providers) = self.mime_routes.get(&mime_key) {
            if let Some(provider) = providers.first() {
                return Some(provider.clone());
            }
        }

        if let Some(ext) = filename
            .and_then(|f| std::path::Path::new(f).extension().and_then(|s| s.to_str()))
            .map(|s| s.to_lowercase())
        {
            if let Some(providers) = self.ext_routes.get(&ext) {
                if let Some(provider) = providers.first() {
                    return Some(provider.clone());
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use xworkflow_types::{DocumentMetadata, ExtractionResult};

    struct MockExtractorProvider;

    #[async_trait]
    impl DocumentExtractorProvider for MockExtractorProvider {
        fn name(&self) -> &str {
            "mock"
        }

        fn priority(&self) -> i32 {
            10
        }

        fn supported_mime_types(&self) -> Vec<&str> {
            vec!["text/plain"]
        }

        fn supported_extensions(&self) -> Vec<&str> {
            vec!["txt"]
        }

        async fn extract(&self, request: ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
            Ok(ExtractionResult {
                text: String::from_utf8_lossy(&request.content).to_string(),
                metadata: DocumentMetadata {
                    page_count: None,
                    sheet_count: None,
                    encoding: Some("utf-8".into()),
                    extractor_used: "mock".into(),
                },
            })
        }
    }

    #[test]
    fn test_document_extractor_executor_new() {
        let providers: Vec<Arc<dyn DocumentExtractorProvider>> = vec![
            Arc::new(MockExtractorProvider),
        ];
        let router = Arc::new(ExtractorRouter::from_providers(&providers));
        let executor = DocumentExtractorExecutor::new(router);
        assert!(Arc::strong_count(&executor.router) >= 1);
    }

    #[test]
    fn test_document_extractor_executor_new_with_providers() {
        let providers: Vec<Arc<dyn DocumentExtractorProvider>> = vec![
            Arc::new(MockExtractorProvider),
        ];
        let executor = DocumentExtractorExecutor::new_with_providers(providers);
        assert!(Arc::strong_count(&executor.router) >= 1);
    }

    #[tokio::test]
    async fn test_execute_no_variables() {
        let providers: Vec<Arc<dyn DocumentExtractorProvider>> = vec![
            Arc::new(MockExtractorProvider),
        ];
        let executor = DocumentExtractorExecutor::new_with_providers(providers);
        
        let config = serde_json::json!({});
        let pool = VariablePool::new();
        let context = RuntimeContext::default();
        
        let result = executor.execute("test", &config, &pool, &context).await;
        assert!(result.is_err());
        match result {
            Err(NodeError::ConfigError(msg)) => {
                assert!(msg.contains("requires variables"));
            }
            _ => panic!("Expected ConfigError"),
        }
    }

    #[tokio::test]
    async fn test_execute_no_files() {
        let providers: Vec<Arc<dyn DocumentExtractorProvider>> = vec![
            Arc::new(MockExtractorProvider),
        ];
        let executor = DocumentExtractorExecutor::new_with_providers(providers);
        
        let config = serde_json::json!({
            "variables": [
                {"value_selector": ["test", "file"]}
            ]
        });
        let pool = VariablePool::new();
        let context = RuntimeContext::default();
        
        let result = executor.execute("test", &config, &pool, &context).await;
        // The exact error type depends on what segment type is returned from the empty pool.
        // When the variable doesn't exist, VariablePool returns Segment::None, which will fail
        // in segment_to_files with either TypeError or in the file validation with InputValidationError.
        // Both outcomes are valid error conditions we're testing for.
        assert!(result.is_err());
    }

    #[test]
    fn test_segment_to_files_file() {
        let file = FileSegment {
            name: "test.txt".into(),
            mime_type: "text/plain".into(),
            size: 100,
            id: Some("test".into()),
            ..Default::default()
        };
        let segment = Segment::File(Arc::new(file.clone()));
        
        let result = segment_to_files(&segment);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn test_segment_to_files_array_file() {
        let file1 = FileSegment {
            name: "test1.txt".into(),
            mime_type: "text/plain".into(),
            size: 100,
            id: Some("test1".into()),
            ..Default::default()
        };
        let file2 = FileSegment {
            name: "test2.txt".into(),
            mime_type: "text/plain".into(),
            size: 200,
            id: Some("test2".into()),
            ..Default::default()
        };
        let segment = Segment::ArrayFile(Arc::new(vec![file1, file2]));
        
        let result = segment_to_files(&segment);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 2);
    }

    #[test]
    fn test_segment_to_files_none() {
        let segment = Segment::None;
        let result = segment_to_files(&segment);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_segment_to_files_invalid() {
        let segment = Segment::String("not a file".into());
        let result = segment_to_files(&segment);
        assert!(result.is_err());
        match result {
            Err(NodeError::TypeError(msg)) => {
                assert!(msg.contains("expects file"));
            }
            _ => panic!("Expected TypeError"),
        }
    }

    #[test]
    fn test_detect_mime_type_from_file() {
        let file = FileSegment {
            mime_type: "application/pdf".into(),
            ..Default::default()
        };
        let content = vec![];
        
        let mime = detect_mime_type(&file, &content);
        assert_eq!(mime, "application/pdf");
    }

    #[test]
    fn test_detect_mime_type_from_filename() {
        let file = FileSegment {
            name: "document.pdf".into(),
            ..Default::default()
        };
        let content = vec![];
        
        let mime = detect_mime_type(&file, &content);
        // Since mime_type is empty, it will try mime_guess which should work for .pdf
        assert!(mime.contains("pdf") || mime == "application/octet-stream");
    }

    #[test]
    fn test_detect_mime_type_default() {
        let file = FileSegment::default();
        let content = vec![];
        
        let mime = detect_mime_type(&file, &content);
        assert_eq!(mime, "application/octet-stream");
    }

    #[test]
    fn test_build_outputs_single() {
        let texts = vec!["test text".into()];
        let metas = vec![serde_json::json!({"key": "value"})];
        
        let result = build_outputs(texts, metas, false);
        assert!(matches!(result.status, WorkflowNodeExecutionStatus::Succeeded));
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            assert!(outputs.contains_key("text"));
            assert!(outputs.contains_key("metadata"));
        } else {
            panic!("Expected Sync outputs");
        }
    }

    #[test]
    fn test_build_outputs_array() {
        let texts = vec!["text1".into(), "text2".into()];
        let metas = vec![
            serde_json::json!({"key": "value1"}),
            serde_json::json!({"key": "value2"}),
        ];
        
        let result = build_outputs(texts, metas, true);
        assert!(matches!(result.status, WorkflowNodeExecutionStatus::Succeeded));
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            assert!(outputs.contains_key("text"));
            assert!(outputs.contains_key("metadata"));
        } else {
            panic!("Expected Sync outputs");
        }
    }

    #[test]
    fn test_enforce_output_limit_ok() {
        let result = enforce_output_limit("test", 1000, 500);
        assert!(result.is_ok());
    }

    #[test]
    fn test_enforce_output_limit_exceeded() {
        let result = enforce_output_limit("test", 100, 200);
        assert!(result.is_err());
        match result {
            Err(NodeError::OutputTooLarge { max, actual, .. }) => {
                assert_eq!(max, 100);
                assert_eq!(actual, 200);
            }
            _ => panic!("Expected OutputTooLarge error"),
        }
    }

    #[test]
    fn test_error_metadata() {
        let file = FileSegment {
            name: "test.txt".into(),
            mime_type: "text/plain".into(),
            size: 100,
            ..Default::default()
        };
        let err = ExtractError::Timeout { seconds: 30 };
        
        let meta = error_metadata(&file, &err);
        assert!(meta.is_object());
        let obj = meta.as_object().unwrap();
        assert!(obj.contains_key("error"));
    }

    #[test]
    fn test_map_extract_error_unsupported_format() {
        let err = ExtractError::UnsupportedFormat {
            mime_type: "unknown/type".into(),
            filename: None,
        };
        let result = map_extract_error(err);
        
        // Verify the error message contains the expected information
        let error_msg = result.to_string();
        assert!(error_msg.contains("UnsupportedFormat") || error_msg.contains("unsupported"));
    }

    #[test]
    fn test_extractor_router_from_providers() {
        let providers: Vec<Arc<dyn DocumentExtractorProvider>> = vec![
            Arc::new(MockExtractorProvider),
        ];
        let router = ExtractorRouter::from_providers(&providers);
        
        assert!(!router.mime_routes.is_empty());
        assert!(!router.ext_routes.is_empty());
    }

    #[test]
    fn test_extractor_router_route_by_mime() {
        let providers: Vec<Arc<dyn DocumentExtractorProvider>> = vec![
            Arc::new(MockExtractorProvider),
        ];
        let router = ExtractorRouter::from_providers(&providers);
        
        let provider = router.route("text/plain", None);
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().name(), "mock");
    }

    #[test]
    fn test_extractor_router_route_by_extension() {
        let providers: Vec<Arc<dyn DocumentExtractorProvider>> = vec![
            Arc::new(MockExtractorProvider),
        ];
        let router = ExtractorRouter::from_providers(&providers);
        
        let provider = router.route("unknown/type", Some("test.txt"));
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().name(), "mock");
    }

    #[test]
    fn test_extractor_router_route_not_found() {
        let providers: Vec<Arc<dyn DocumentExtractorProvider>> = vec![
            Arc::new(MockExtractorProvider),
        ];
        let router = ExtractorRouter::from_providers(&providers);
        
        let provider = router.route("unknown/type", None);
        assert!(provider.is_none());
    }

    #[test]
    fn test_max_output_limit() {
        let context = RuntimeContext::default();
        let limit = max_output_limit(&context, "test");
        assert_eq!(limit, 10 * 1024 * 1024);
    }
}
