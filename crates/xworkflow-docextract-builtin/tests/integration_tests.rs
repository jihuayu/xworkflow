use xworkflow_docextract_builtin::BuiltinDocExtractProvider;
use xworkflow_types::{
    DocumentExtractorProvider, ExtractionRequest, OutputFormat,
};
use serde_json::Value;

#[tokio::test]
async fn test_extract_plain_text() {
    let provider = BuiltinDocExtractProvider::new();
    let content = b"Hello, World!\nThis is a test.";
    
    let request = ExtractionRequest {
        content: content.to_vec(),
        mime_type: "text/plain".to_string(),
        filename: Some("test.txt".to_string()),
        output_format: OutputFormat::Text,
        options: Value::Object(Default::default()),
    };
    
    let result = provider.extract(request).await.unwrap();
    assert!(result.text.contains("Hello, World!"));
    assert!(result.text.contains("This is a test."));
}

#[tokio::test]
async fn test_extract_json() {
    let provider = BuiltinDocExtractProvider::new();
    let content = br#"{"name": "test", "value": 42}"#;
    
    let request = ExtractionRequest {
        content: content.to_vec(),
        mime_type: "application/json".to_string(),
        filename: Some("test.json".to_string()),
        output_format: OutputFormat::Text,
        options: Value::Object(Default::default()),
    };
    
    let result = provider.extract(request).await.unwrap();
    assert!(result.text.contains("name"));
    assert!(result.text.contains("test"));
    assert!(result.text.contains("42"));
}

#[tokio::test]
async fn test_extract_yaml() {
    let provider = BuiltinDocExtractProvider::new();
    let content = b"name: test\nvalue: 42";
    
    let request = ExtractionRequest {
        content: content.to_vec(),
        mime_type: "application/yaml".to_string(),
        filename: Some("test.yaml".to_string()),
        output_format: OutputFormat::Text,
        options: Value::Object(Default::default()),
    };
    
    let result = provider.extract(request).await.unwrap();
    assert!(result.text.contains("name"));
    assert!(result.text.contains("test"));
}

#[tokio::test]
async fn test_supported_types() {
    let provider = BuiltinDocExtractProvider::new();
    let mime_types = provider.supported_mime_types();
    
    assert!(mime_types.contains(&"text/plain"));
    assert!(mime_types.contains(&"application/json"));
    assert!(mime_types.contains(&"text/csv"));
}

#[tokio::test]
async fn test_supported_extensions() {
    let provider = BuiltinDocExtractProvider::new();
    let extensions = provider.supported_extensions();
    
    assert!(extensions.contains(&"txt"));
    assert!(extensions.contains(&"json"));
}

#[tokio::test]
async fn test_provider_name() {
    let provider = BuiltinDocExtractProvider::new();
    assert_eq!(provider.name(), "builtin-doc-extract");
}
