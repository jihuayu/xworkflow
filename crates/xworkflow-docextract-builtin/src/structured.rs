use std::io::Cursor;

use quick_xml::events::Event;
use quick_xml::Reader;
use xworkflow_types::{ExtractError, ExtractionRequest, ExtractionResult};

use crate::{extraction_failed, metadata};

pub fn extract_json(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let value: serde_json::Value = serde_json::from_slice(&request.content)
        .map_err(|e| extraction_failed("json", e.to_string()))?;
    let text = serde_json::to_string_pretty(&value)
        .map_err(|e| extraction_failed("json", e.to_string()))?;

    Ok(ExtractionResult {
        text,
        metadata: metadata("builtin-json", None),
    })
}

pub fn extract_yaml(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let value: serde_yaml::Value = serde_yaml::from_slice(&request.content)
        .map_err(|e| extraction_failed("yaml", e.to_string()))?;
    let text =
        serde_yaml::to_string(&value).map_err(|e| extraction_failed("yaml", e.to_string()))?;

    Ok(ExtractionResult {
        text,
        metadata: metadata("builtin-yaml", None),
    })
}

pub fn extract_xml(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let mut reader = Reader::from_reader(Cursor::new(&request.content));
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut out = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().to_string();
                if !text.is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&text);
                }
            }
            Ok(Event::CData(e)) => {
                let text = String::from_utf8_lossy(e.as_ref()).to_string();
                if !text.is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&text);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(extraction_failed("xml", e.to_string())),
            _ => {}
        }
        buf.clear();
    }

    Ok(ExtractionResult {
        text: out,
        metadata: metadata("builtin-xml", None),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xworkflow_types::OutputFormat;

    #[test]
    fn test_extract_json_pretty() {
        let request = ExtractionRequest {
            content: br#"{"a":1,"b":"x"}"#.to_vec(),
            mime_type: "application/json".to_string(),
            filename: Some("sample.json".to_string()),
            output_format: OutputFormat::Text,
            options: serde_json::json!({}),
        };

        let result = extract_json(&request).unwrap();
        assert!(result.text.contains("\"a\": 1"));
        assert!(result.text.contains("\"b\": \"x\""));
    }

    #[test]
    fn test_extract_xml_text_nodes() {
        let request = ExtractionRequest {
            content: br#"<root><a>hello</a><b>world</b></root>"#.to_vec(),
            mime_type: "application/xml".to_string(),
            filename: Some("sample.xml".to_string()),
            output_format: OutputFormat::Text,
            options: serde_json::json!({}),
        };

        let result = extract_xml(&request).unwrap();
        assert!(result.text.contains("hello"));
        assert!(result.text.contains("world"));
    }
}
