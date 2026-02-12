use xworkflow_types::{ExtractError, ExtractionRequest, ExtractionResult, OutputFormat};

use crate::encoding::decode_text;
use crate::{extraction_failed, metadata};

pub fn extract_text(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let encoding = request
        .options
        .get("encoding")
        .and_then(|v| v.as_str());
    let decoded = decode_text(&request.content, encoding)?;

    let mut text = decoded.text;
    if request.output_format == OutputFormat::Markdown {
        if let Some(lang) = language_from_filename(request.filename.as_deref()) {
            text = wrap_code_block(&text, lang);
        }
    }

    Ok(ExtractionResult {
        text,
        metadata: metadata("builtin-text", decoded.encoding),
    })
}

pub fn extract_rtf(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let encoding = request
        .options
        .get("encoding")
        .and_then(|v| v.as_str());
    let decoded = decode_text(&request.content, encoding)?;
    let text = strip_rtf(&decoded.text);

    Ok(ExtractionResult {
        text,
        metadata: metadata("builtin-rtf", decoded.encoding),
    })
}

fn wrap_code_block(text: &str, language: &str) -> String {
    let mut out = String::new();
    out.push_str("```");
    out.push_str(language);
    out.push('\n');
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");
    out
}

fn language_from_filename(filename: Option<&str>) -> Option<&'static str> {
    let ext = filename
        .and_then(|f| std::path::Path::new(f).extension().and_then(|s| s.to_str()))?
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "js" => Some("javascript"),
        "ts" => Some("typescript"),
        "go" => Some("go"),
        "java" => Some("java"),
        "cpp" | "cc" | "cxx" => Some("cpp"),
        "c" => Some("c"),
        "h" | "hpp" | "hh" => Some("cpp"),
        "cs" => Some("csharp"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "swift" => Some("swift"),
        "kt" => Some("kotlin"),
        "scala" => Some("scala"),
        "sql" => Some("sql"),
        "sh" | "bash" => Some("bash"),
        "ps1" => Some("powershell"),
        "toml" => Some("toml"),
        "ini" | "cfg" | "conf" => Some("ini"),
        _ => None,
    }
}

fn strip_rtf(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' | '}' => continue,
            '\\' => {
                if let Some(next) = chars.peek().copied() {
                    if next == '\\' || next == '{' || next == '}' {
                        out.push(next);
                        chars.next();
                        continue;
                    }
                }
                while let Some(next) = chars.peek().copied() {
                    if next.is_ascii_alphabetic() || next.is_ascii_digit() || next == '-' {
                        chars.next();
                        continue;
                    }
                    if next == ' ' {
                        chars.next();
                    }
                    break;
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

#[allow(dead_code)]
fn unexpected_format(format: &str, message: &str) -> ExtractError {
    extraction_failed(format, message)
}
