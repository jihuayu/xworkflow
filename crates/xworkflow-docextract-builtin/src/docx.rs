use std::io::Read;

use quick_xml::events::Event;
use quick_xml::Reader;
use xworkflow_types::{ExtractError, ExtractionRequest, ExtractionResult, OutputFormat};

use crate::table::{render_markdown_table, render_text_table};
use crate::{extraction_failed, metadata};

pub fn extract_docx(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&request.content))
        .map_err(|e| ExtractError::CorruptedFile(e.to_string()))?;

    let mut file = archive
        .by_name("word/document.xml")
        .map_err(|e| ExtractError::CorruptedFile(e.to_string()))?;

    let mut xml = String::new();
    file.read_to_string(&mut xml)
        .map_err(|e| ExtractError::ExtractionFailed {
            format: "docx".to_string(),
            message: e.to_string(),
        })?;

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut blocks: Vec<String> = Vec::new();

    let mut in_paragraph = false;
    let mut in_table = false;
    let mut current_paragraph = String::new();
    let mut current_cell = String::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut heading_level: Option<usize> = None;
    let mut is_list_item = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                match e.name().as_ref() {
                    b"w:p" => {
                        in_paragraph = true;
                        current_paragraph.clear();
                        heading_level = None;
                        is_list_item = false;
                    }
                    b"w:pStyle" => {
                        if let Some(level) = parse_heading_level(&e) {
                            heading_level = Some(level);
                        }
                    }
                    b"w:numPr" => {
                        is_list_item = true;
                    }
                    b"w:tbl" => {
                        in_table = true;
                        table_rows.clear();
                    }
                    b"w:tr" => {
                        current_row.clear();
                    }
                    b"w:tc" => {
                        current_cell.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().to_string();
                if in_table {
                    current_cell.push_str(&text);
                } else if in_paragraph {
                    current_paragraph.push_str(&text);
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:tc" => {
                    current_row.push(current_cell.trim().to_string());
                    current_cell.clear();
                }
                b"w:tr" => {
                    if !current_row.is_empty() {
                        table_rows.push(current_row.clone());
                        current_row.clear();
                    }
                }
                b"w:tbl" => {
                    in_table = false;
                    if !table_rows.is_empty() {
                        let table = match request.output_format {
                            OutputFormat::Markdown => render_markdown_table(&table_rows, true),
                            OutputFormat::Text => render_text_table(&table_rows),
                        };
                        if !table.is_empty() {
                            blocks.push(table);
                        }
                    }
                }
                b"w:p" => {
                    in_paragraph = false;
                    let line = current_paragraph.trim();
                    if !line.is_empty() {
                        let rendered = match request.output_format {
                            OutputFormat::Markdown => render_markdown_paragraph(
                                line,
                                heading_level,
                                is_list_item,
                            ),
                            OutputFormat::Text => line.to_string(),
                        };
                        blocks.push(rendered);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(extraction_failed("docx", e.to_string()));
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(ExtractionResult {
        text: blocks.join("\n\n"),
        metadata: metadata("builtin-docx", None),
    })
}

fn render_markdown_paragraph(text: &str, heading_level: Option<usize>, list_item: bool) -> String {
    if let Some(level) = heading_level {
        let mut prefix = String::new();
        for _ in 0..level {
            prefix.push('#');
        }
        return format!("{} {}", prefix, text);
    }
    if list_item {
        return format!("- {}", text);
    }
    text.to_string()
}

fn parse_heading_level(e: &quick_xml::events::BytesStart) -> Option<usize> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == b"w:val" || attr.key.as_ref() == b"val" {
            if let Ok(val) = attr.unescape_value() {
                let val = val.as_ref();
                if let Some(level) = val.strip_prefix("Heading") {
                    if let Ok(n) = level.parse::<usize>() {
                        return Some(n);
                    }
                }
            }
        }
    }
    None
}
