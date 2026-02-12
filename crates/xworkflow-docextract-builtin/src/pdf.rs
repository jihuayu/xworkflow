use xworkflow_types::{ExtractError, ExtractionRequest, ExtractionResult, OutputFormat};

use crate::{extraction_failed, metadata};

pub fn extract_pdf(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let text = pdf_extract::extract_text_from_mem(&request.content)
        .map_err(|e| map_pdf_error(e.to_string()))?;

    let pages = split_pages(&text);
    let total_pages = pages.len() as u32;

    let page_range = request
        .options
        .get("pdf")
        .and_then(|v| v.get("page_range"))
        .and_then(|v| v.as_str());

    let selected_pages = if let Some(range_str) = page_range {
        parse_page_ranges(range_str, pages.len())?
    } else {
        (1..=pages.len()).collect()
    };

    let separator = match request.output_format {
        OutputFormat::Text => "\n\n",
        OutputFormat::Markdown => "\n\n---\n\n",
    };

    let mut out = String::new();
    for (idx, page_idx) in selected_pages.iter().enumerate() {
        if let Some(page) = pages.get(page_idx.saturating_sub(1)) {
            if idx > 0 {
                out.push_str(separator);
            }
            out.push_str(page);
        }
    }

    let mut meta = metadata("builtin-pdf", None);
    meta.page_count = Some(total_pages);

    Ok(ExtractionResult {
        text: out,
        metadata: meta,
    })
}

fn split_pages(text: &str) -> Vec<String> {
    let mut pages: Vec<String> = text
        .split('\u{0c}')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if pages.is_empty() {
        pages.push(text.trim().to_string());
    }
    pages
}

fn parse_page_ranges(input: &str, max_pages: usize) -> Result<Vec<usize>, ExtractError> {
    let mut out = Vec::new();
    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start = start.trim().parse::<usize>().map_err(|_| {
                extraction_failed("pdf", format!("invalid page range: {input}"))
            })?;
            let end = end.trim().parse::<usize>().map_err(|_| {
                extraction_failed("pdf", format!("invalid page range: {input}"))
            })?;
            if start == 0 || end == 0 || start > end {
                return Err(extraction_failed(
                    "pdf",
                    format!("invalid page range: {input}"),
                ));
            }
            for page in start..=end {
                if page <= max_pages {
                    out.push(page);
                }
            }
        } else {
            let page = part.parse::<usize>().map_err(|_| {
                extraction_failed("pdf", format!("invalid page range: {input}"))
            })?;
            if page == 0 {
                return Err(extraction_failed(
                    "pdf",
                    format!("invalid page range: {input}"),
                ));
            }
            if page <= max_pages {
                out.push(page);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn map_pdf_error(message: String) -> ExtractError {
    let lower = message.to_lowercase();
    if lower.contains("password") || lower.contains("encrypted") {
        ExtractError::PasswordProtected
    } else if lower.contains("corrupt") {
        ExtractError::CorruptedFile(message)
    } else {
        ExtractError::ExtractionFailed {
            format: "pdf".to_string(),
            message,
        }
    }
}
