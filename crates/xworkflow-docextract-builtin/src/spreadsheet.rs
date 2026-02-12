use std::io::Cursor;

use calamine::{Data, Reader};
use xworkflow_types::{ExtractError, ExtractionRequest, ExtractionResult, OutputFormat};

use crate::table::{render_markdown_table, render_text_table};
use crate::{extraction_failed, metadata};

pub fn extract_csv(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(Cursor::new(&request.content));

    let options = request
        .options
        .get("spreadsheet")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let include_header = options
        .get("include_header")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let max_rows = options
        .get("max_rows")
        .and_then(|v| v.as_u64())
        .unwrap_or(10_000) as usize;

    let mut rows: Vec<Vec<String>> = Vec::new();
    for record in reader.records().take(max_rows) {
        let record = record.map_err(|e| extraction_failed("csv", e.to_string()))?;
        rows.push(record.iter().map(|s| s.to_string()).collect());
    }

    let text = match request.output_format {
        OutputFormat::Markdown => render_markdown_table(&rows, include_header),
        OutputFormat::Text => render_text_table(&rows),
    };

    Ok(ExtractionResult {
        text,
        metadata: metadata("builtin-csv", None),
    })
}

pub fn extract_spreadsheet(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let ext = request
        .filename
        .as_deref()
        .and_then(|f| std::path::Path::new(f).extension().and_then(|s| s.to_str()))
        .unwrap_or("")
        .to_ascii_lowercase();

    let options = request
        .options
        .get("spreadsheet")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let include_header = options
        .get("include_header")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let max_rows = options
        .get("max_rows")
        .and_then(|v| v.as_u64())
        .unwrap_or(10_000) as usize;
    let sheet_index = options
        .get("sheet_index")
        .and_then(|v| v.as_i64());

    let mut blocks: Vec<String> = Vec::new();
    let mut sheet_count = 0u32;

    match ext.as_str() {
        "xlsx" => {
            let mut workbook = calamine::Xlsx::new(Cursor::new(&request.content))
                .map_err(|e| extraction_failed("xlsx", e.to_string()))?;
            let names = workbook.sheet_names().to_vec();
            sheet_count = names.len() as u32;
            for (idx, name) in names.iter().enumerate() {
                if let Some(target) = sheet_index {
                    if target as usize != idx {
                        continue;
                    }
                }
                if let Ok(range) = workbook.worksheet_range(name) {
                    let table = range_to_table(range, include_header, max_rows, request.output_format);
                    if !table.is_empty() {
                        if request.output_format == OutputFormat::Markdown {
                            blocks.push(format!("### {}\n\n{}", name, table));
                        } else {
                            blocks.push(format!("{}\n{}", name, table));
                        }
                    }
                }
            }
        }
        "xls" => {
            let mut workbook = calamine::Xls::new(Cursor::new(&request.content))
                .map_err(|e| extraction_failed("xls", e.to_string()))?;
            let names = workbook.sheet_names().to_vec();
            sheet_count = names.len() as u32;
            for (idx, name) in names.iter().enumerate() {
                if let Some(target) = sheet_index {
                    if target as usize != idx {
                        continue;
                    }
                }
                if let Ok(range) = workbook.worksheet_range(name) {
                    let table = range_to_table(range, include_header, max_rows, request.output_format);
                    if !table.is_empty() {
                        if request.output_format == OutputFormat::Markdown {
                            blocks.push(format!("### {}\n\n{}", name, table));
                        } else {
                            blocks.push(format!("{}\n{}", name, table));
                        }
                    }
                }
            }
        }
        "ods" => {
            let mut workbook = calamine::Ods::new(Cursor::new(&request.content))
                .map_err(|e| extraction_failed("ods", e.to_string()))?;
            let names = workbook.sheet_names().to_vec();
            sheet_count = names.len() as u32;
            for (idx, name) in names.iter().enumerate() {
                if let Some(target) = sheet_index {
                    if target as usize != idx {
                        continue;
                    }
                }
                if let Ok(range) = workbook.worksheet_range(name) {
                    let table = range_to_table(range, include_header, max_rows, request.output_format);
                    if !table.is_empty() {
                        if request.output_format == OutputFormat::Markdown {
                            blocks.push(format!("### {}\n\n{}", name, table));
                        } else {
                            blocks.push(format!("{}\n{}", name, table));
                        }
                    }
                }
            }
        }
        _ => return Err(ExtractError::UnsupportedFormat {
            mime_type: request.mime_type.clone(),
            filename: request.filename.clone(),
        }),
    }

    let mut meta = metadata("builtin-spreadsheet", None);
    meta.sheet_count = Some(sheet_count);

    Ok(ExtractionResult {
        text: blocks.join("\n\n"),
        metadata: meta,
    })
}

fn range_to_table(
    range: calamine::Range<Data>,
    include_header: bool,
    max_rows: usize,
    output_format: OutputFormat,
) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut total_rows = 0usize;
    for row in range.rows() {
        total_rows += 1;
        if rows.len() >= max_rows {
            break;
        }
        rows.push(row.iter().map(cell_to_string).collect());
    }

    let mut text = match output_format {
        OutputFormat::Markdown => render_markdown_table(&rows, include_header),
        OutputFormat::Text => render_text_table(&rows),
    };

    if total_rows > max_rows {
        let remaining = total_rows - max_rows;
        text.push_str(&format!("\n... (truncated, {} more rows)", remaining));
    }

    text
}

fn cell_to_string(cell: &Data) -> String {
    cell.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_csv_markdown() {
        let content = "name,age\nAlice,30\nBob,40\n";
        let request = ExtractionRequest {
            content: content.as_bytes().to_vec(),
            mime_type: "text/csv".to_string(),
            filename: Some("sample.csv".to_string()),
            output_format: OutputFormat::Markdown,
            options: json!({
                "spreadsheet": {
                    "include_header": true,
                    "max_rows": 100
                }
            }),
        };

        let result = extract_csv(&request).unwrap();
        assert!(result.text.contains("| name | age |"));
        assert!(result.text.contains("Alice"));
        assert!(result.text.contains("Bob"));
    }
}
