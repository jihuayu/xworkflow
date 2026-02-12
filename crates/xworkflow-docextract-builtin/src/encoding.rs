use encoding_rs::Encoding;

use xworkflow_types::ExtractError;

pub struct DecodedText {
    pub text: String,
    pub encoding: Option<String>,
}

pub fn decode_text(content: &[u8], preferred: Option<&str>) -> Result<DecodedText, ExtractError> {
    if let Some(label) = preferred {
        let encoding = Encoding::for_label(label.as_bytes())
            .ok_or_else(|| ExtractError::EncodingError(format!("unknown encoding: {label}")))?;
        let (text, _, _) = encoding.decode(content);
        return Ok(DecodedText {
            text: text.to_string(),
            encoding: Some(encoding.name().to_lowercase()),
        });
    }

    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(content, true);
    let encoding = detector.guess(None, true);
    let (text, _, had_errors) = encoding.decode(content);
    if !had_errors {
        return Ok(DecodedText {
            text: text.to_string(),
            encoding: Some(encoding.name().to_lowercase()),
        });
    }

    if let Ok(text) = String::from_utf8(content.to_vec()) {
        return Ok(DecodedText {
            text,
            encoding: Some("utf-8".to_string()),
        });
    }

    let (text, _, _) = encoding_rs::ISO_8859_1.decode(content);
    Ok(DecodedText {
        text: text.to_string(),
        encoding: Some("iso-8859-1".to_string()),
    })
}
