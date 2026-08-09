use anyhow::Result;

use super::decode_text;
use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

const MIMETYPES: &[&str] = &["text/csv", "text/tab-separated-values"];

pub struct CsvConverter;

impl Converter for CsvConverter {
    fn name(&self) -> &'static str {
        "csv"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        matches!(info.extension.as_deref(), Some(".csv") | Some(".tsv"))
            || info
                .mimetype
                .as_deref()
                .is_some_and(|m| MIMETYPES.iter().any(|p| m.starts_with(p)))
    }

    fn convert(
        &self,
        input: &[u8],
        info: &StreamInfo,
        _options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let text = decode_text(input);
        let delimiter = if info.extension.as_deref() == Some(".tsv") {
            '\t'
        } else {
            ','
        };
        let rows = parse_rows(&text, delimiter);

        if rows.is_empty() {
            return Ok(ConversionResult::markdown(""));
        }

        let header = &rows[0];
        let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 1);

        lines.push(format!("| {} |", header.join(" | ")));
        lines.push(format!(
            "| {} |",
            vec!["---"; header.len()].join(" | ")
        ));

        for row in &rows[1..] {
            let mut row = row.clone();
            // Pad row to match header length.
            while row.len() < header.len() {
                row.push(String::new());
            }
            lines.push(format!("| {} |", row.join(" | ")));
        }

        Ok(ConversionResult::markdown(lines.join("\n")))
    }
}

/// Minimal CSV parser: quoted fields, escaped quotes, CRLF, trimmed cells,
/// empty rows skipped. Mirrors the TS implementation exactly.
fn parse_rows(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut in_quotes = false;

    let mut push_row = |current: &mut Vec<String>, cell: &mut String| {
        current.push(cell.trim().to_string());
        cell.clear();
        if current.iter().any(|c| !c.is_empty()) {
            rows.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };

    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if in_quotes {
            if ch == '"' && chars.get(i + 1) == Some(&'"') {
                cell.push('"');
                i += 1;
            } else if ch == '"' {
                in_quotes = false;
            } else {
                cell.push(ch);
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == delimiter {
            current.push(cell.trim().to_string());
            cell.clear();
        } else if ch == '\n' || (ch == '\r' && chars.get(i + 1) == Some(&'\n')) {
            push_row(&mut current, &mut cell);
            if ch == '\r' {
                i += 1;
            }
        } else {
            cell.push(ch);
        }
        i += 1;
    }

    // Last row.
    if !cell.is_empty() || !current.is_empty() {
        push_row(&mut current, &mut cell);
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Converter, MarkitOptions, StreamInfo};

    fn info(ext: &str) -> StreamInfo {
        StreamInfo { extension: Some(ext.to_string()), ..Default::default() }
    }

    #[test]
    fn quoted_fields_and_escaped_quotes() {
        let input = "name,age,\"city, state\"\nAlice,30,\"Portland, OR\"\n\"Q \"\"quoted\"\"\",1,\n\n";
        let result = CsvConverter
            .convert(input.as_bytes(), &info(".csv"), &MarkitOptions::default())
            .unwrap();
        assert_eq!(
            result.markdown,
            "| name | age | city, state |\n| --- | --- | --- |\n| Alice | 30 | Portland, OR |\n| Q \"quoted\" | 1 |  |"
        );
    }

    #[test]
    fn tsv_delimiter() {
        let result = CsvConverter
            .convert(b"a\tb\n1\t2\n", &info(".tsv"), &MarkitOptions::default())
            .unwrap();
        assert_eq!(result.markdown, "| a | b |\n| --- | --- |\n| 1 | 2 |");
    }

    #[test]
    fn empty_input() {
        let result = CsvConverter
            .convert(b"", &info(".csv"), &MarkitOptions::default())
            .unwrap();
        assert_eq!(result.markdown, "");
    }
}
