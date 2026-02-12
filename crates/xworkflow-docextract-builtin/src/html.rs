use std::io::Cursor;

use xworkflow_types::{ExtractError, ExtractionRequest, ExtractionResult};

use crate::{metadata};

pub fn extract_html(request: &ExtractionRequest) -> Result<ExtractionResult, ExtractError> {
    let text = html2text::from_read(Cursor::new(&request.content), 80);

    Ok(ExtractionResult {
        text,
        metadata: metadata("builtin-html", None),
    })
}
