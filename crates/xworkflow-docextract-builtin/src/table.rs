pub fn render_markdown_table(rows: &[Vec<String>], include_header: bool) -> String {
    if rows.is_empty() {
        return String::new();
    }

    let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if max_cols == 0 {
        return String::new();
    }

    let (header, data_rows) = if include_header {
        (&rows[0], &rows[1..])
    } else {
        (&vec![String::new(); max_cols], rows)
    };

    let mut out = String::new();
    out.push('|');
    for i in 0..max_cols {
        let val = header.get(i).map(|s| s.as_str()).unwrap_or("");
        out.push(' ');
        out.push_str(val);
        out.push(' ');
        out.push('|');
    }
    out.push('\n');

    out.push('|');
    for _ in 0..max_cols {
        out.push_str(" --- |");
    }
    out.push('\n');

    for row in data_rows {
        out.push('|');
        for i in 0..max_cols {
            let val = row.get(i).map(|s| s.as_str()).unwrap_or("");
            out.push(' ');
            out.push_str(val);
            out.push(' ');
            out.push('|');
        }
        out.push('\n');
    }

    out
}

pub fn render_text_table(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for (idx, row) in rows.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&row.join("\t"));
    }
    out
}
