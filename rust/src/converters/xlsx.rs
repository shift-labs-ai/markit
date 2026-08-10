use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::io::{Cursor, Read, Seek};
use zip::ZipArchive;

use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

const EXTENSIONS: &[&str] = &[".xlsx"];
const MIMETYPES: &[&str] = &["application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"];

/// Namespace URI for the officeDocument relationships (used for r:id in workbook.xml).
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

pub struct XlsxConverter;

impl Converter for XlsxConverter {
    fn name(&self) -> &'static str {
        "xlsx"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ext) = &info.extension {
            if EXTENSIONS.contains(&ext.as_str()) {
                return true;
            }
        }
        if let Some(mime) = &info.mimetype {
            if MIMETYPES.iter().any(|m| mime.starts_with(m)) {
                return true;
            }
        }
        false
    }

    fn convert(
        &self,
        input: &[u8],
        _info: &StreamInfo,
        _options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let cursor = Cursor::new(input);
        let mut archive = ZipArchive::new(cursor)?;

        // Parse shared strings
        let shared = match read_zip_str(&mut archive, "xl/sharedStrings.xml")? {
            Some(xml) => parse_shared_strings(&xml)?,
            None => Vec::new(),
        };

        // Parse workbook for sheet names
        let wb_xml = read_zip_str(&mut archive, "xl/workbook.xml")?
            .ok_or_else(|| anyhow!("Invalid XLSX: missing workbook.xml"))?;
        let sheets = parse_workbook_sheets(&wb_xml)?;

        // Parse workbook rels to map rIds to sheet file targets
        let rel_map = match read_zip_str(&mut archive, "xl/_rels/workbook.xml.rels")? {
            Some(xml) => parse_rels(&xml)?,
            None => HashMap::new(),
        };

        let mut sections: Vec<String> = Vec::new();

        for (sheet_name, r_id) in &sheets {
            let target = match rel_map.get(r_id.as_str()) {
                Some(t) => t,
                None => continue,
            };

            let sheet_path = if let Some(stripped) = target.strip_prefix('/') {
                stripped.to_string()
            } else {
                format!("xl/{}", target)
            };

            let sheet_xml = match read_zip_str(&mut archive, &sheet_path)? {
                Some(xml) => xml,
                None => continue,
            };

            let mut table_rows = parse_sheet_rows(&sheet_xml, &shared)?;
            if table_rows.is_empty() {
                continue;
            }

            // Normalize column count — pad all rows to max width
            let max_cols = table_rows.iter().map(|r| r.len()).max().unwrap_or(0);
            for row in &mut table_rows {
                while row.len() < max_cols {
                    row.push(String::new());
                }
            }

            sections.push(format!("## {}", sheet_name));

            let header = &table_rows[0];
            let mut lines: Vec<String> = Vec::new();
            lines.push(format!("| {} |", header.join(" | ")));
            lines.push(format!("| {} |", vec!["---"; header.len()].join(" | ")));
            for row in &table_rows[1..] {
                lines.push(format!("| {} |", row.join(" | ")));
            }
            sections.push(lines.join("\n"));
        }

        Ok(ConversionResult::markdown(sections.join("\n\n")))
    }
}

// ---------------------------------------------------------------------------
// ZIP helpers
// ---------------------------------------------------------------------------

/// Read a ZIP entry as a UTF-8 string. Returns None if the entry doesn't exist.
fn read_zip_str<R: Read + Seek>(archive: &mut ZipArchive<R>, name: &str) -> Result<Option<String>> {
    match archive.by_name(name) {
        Ok(mut file) => {
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            Ok(Some(content))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(anyhow!("zip error reading {}: {}", name, e)),
    }
}

// ---------------------------------------------------------------------------
// XML helpers
// ---------------------------------------------------------------------------

/// Collect all text-node children of an element and concatenate them.
fn text_content(node: roxmltree::Node) -> String {
    node.children()
        .filter(|n| n.is_text())
        .map(|n| n.text().unwrap_or(""))
        .collect()
}

/// Extract display text from a <si> element:
///   - simple: <si><t>...</t></si>
///   - rich:   <si><r><t>...</t></r><r><t>...</t></r></si>
fn extract_si_text(si: roxmltree::Node) -> String {
    // Simple: first direct <t> child
    if let Some(t) = si
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "t")
    {
        return text_content(t);
    }
    // Rich: concatenate <t> from each <r> run
    si.children()
        .filter(|n| n.is_element() && n.tag_name().name() == "r")
        .map(|r| {
            r.children()
                .find(|n| n.is_element() && n.tag_name().name() == "t")
                .map(text_content)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("")
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

/// Parse xl/sharedStrings.xml into an indexed Vec of strings.
fn parse_shared_strings(xml: &str) -> Result<Vec<String>> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    Ok(root
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "si")
        .map(extract_si_text)
        .collect())
}

/// Parse xl/workbook.xml -> [(sheet_name, r_id)].
fn parse_workbook_sheets(xml: &str) -> Result<Vec<(String, String)>> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let mut sheets = Vec::new();
    for node in root.descendants() {
        if node.is_element() && node.tag_name().name() == "sheet" {
            let name = node.attribute("name").unwrap_or("").to_string();
            // r:id is a namespaced attribute
            let r_id = node.attribute((REL_NS, "id")).unwrap_or("").to_string();
            if !name.is_empty() && !r_id.is_empty() {
                sheets.push((name, r_id));
            }
        }
    }
    Ok(sheets)
}

/// Parse xl/_rels/workbook.xml.rels -> {Id -> Target}.
fn parse_rels(xml: &str) -> Result<HashMap<String, String>> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let mut map = HashMap::new();
    for node in root.descendants() {
        if node.is_element() && node.tag_name().name() == "Relationship" {
            let id = node.attribute("Id").unwrap_or("").to_string();
            let target = node.attribute("Target").unwrap_or("").to_string();
            if !id.is_empty() {
                map.insert(id, target);
            }
        }
    }
    Ok(map)
}

/// Parse a worksheet XML and return rows as Vec<Vec<String>>.
///
/// Cells are appended in document order -- cell references (r="A1") are
/// intentionally ignored for column gaps, matching the TS implementation.
fn parse_sheet_rows(xml: &str, shared: &[String]) -> Result<Vec<Vec<String>>> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let mut rows = Vec::new();
    for node in root.descendants() {
        if node.is_element() && node.tag_name().name() == "row" {
            let cells: Vec<String> = node
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == "c")
                .map(|c| get_cell_value(c, shared))
                .collect();
            if !cells.is_empty() {
                rows.push(cells);
            }
        }
    }
    Ok(rows)
}

/// Derive the string value of a <c> element.
fn get_cell_value(c: roxmltree::Node, shared: &[String]) -> String {
    let t = c.attribute("t").unwrap_or("");

    match t {
        "s" => {
            // Shared string: <v> contains the 0-based index
            let v = c
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "v")
                .map(text_content)
                .unwrap_or_default();
            let idx: usize = v.trim().parse().unwrap_or(usize::MAX);
            shared.get(idx).cloned().unwrap_or_default()
        }
        "inlineStr" => {
            // Inline string: <is><t>...</t></is> or <is><r><t>...</t></r>...</is>
            match c
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "is")
            {
                Some(is_node) => extract_si_text(is_node),
                None => String::new(),
            }
        }
        "b" => {
            // Boolean
            let v = c
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "v")
                .map(text_content)
                .unwrap_or_default();
            if v.trim() == "1" {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }
        }
        _ => {
            // Number, formula result, date, error, etc. -- <v> as raw string
            c.children()
                .find(|n| n.is_element() && n.tag_name().name() == "v")
                .map(text_content)
                .unwrap_or_default()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MarkitOptions, StreamInfo};
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn info_ext(ext: &str) -> StreamInfo {
        StreamInfo {
            extension: Some(ext.to_string()),
            ..Default::default()
        }
    }

    /// Build a minimal in-memory XLSX buffer.
    ///
    /// The sheet has one <row> with one <c t="s"> per shared string, all in
    /// document order (matching the TS test fixture exactly).
    fn build_xlsx(shared_strings: &[&str]) -> Vec<u8> {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();

        // xl/sharedStrings.xml
        let si_entries: String = shared_strings
            .iter()
            .map(|s| format!("<si><t>{}</t></si>", s))
            .collect();
        let ss_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"{n}\" uniqueCount=\"{n}\">{si}</sst>",
            n = shared_strings.len(),
            si = si_entries,
        );
        zip.start_file("xl/sharedStrings.xml", opts).unwrap();
        zip.write_all(ss_xml.as_bytes()).unwrap();

        // xl/worksheets/sheet1.xml -- one row, one cell per shared string
        let cells: String = shared_strings
            .iter()
            .enumerate()
            .map(|(i, _)| format!("<c r=\"A{}\" t=\"s\"><v>{}</v></c>", i + 1, i))
            .collect();
        let sheet_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData><row r=\"1\">{}</row></sheetData></worksheet>",
            cells,
        );
        zip.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
        zip.write_all(sheet_xml.as_bytes()).unwrap();

        // xl/workbook.xml
        let wb_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Sheet1\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>";
        zip.start_file("xl/workbook.xml", opts).unwrap();
        zip.write_all(wb_xml.as_bytes()).unwrap();

        // xl/_rels/workbook.xml.rels
        let rels_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>";
        zip.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
        zip.write_all(rels_xml.as_bytes()).unwrap();

        zip.finish().unwrap().into_inner()
    }

    // -----------------------------------------------------------------------
    // Mirror the TS test cases
    // -----------------------------------------------------------------------

    /// Mirrors: "handles >1000 entity references without crashing"
    #[test]
    fn test_entity_references_large() {
        let strings: Vec<String> = (0..2000).map(|i| format!("val&amp;ue_{}", i)).collect();
        let str_refs: Vec<&str> = strings.iter().map(|s| s.as_str()).collect();
        let data = build_xlsx(&str_refs);

        let result = XlsxConverter
            .convert(&data, &info_ext(".xlsx"), &MarkitOptions::default())
            .unwrap();
        assert!(
            result.markdown.contains("val&ue_0"),
            "should contain val&ue_0"
        );
        assert!(
            result.markdown.contains("val&ue_999"),
            "should contain val&ue_999"
        );
        assert!(
            result.markdown.contains("val&ue_1999"),
            "should contain val&ue_1999"
        );
    }

    /// Mirrors: "handles shared strings with XML entities"
    #[test]
    fn test_xml_entities() {
        let strings = &[
            "AT&amp;T",
            "x &lt; y",
            "a &gt; b",
            "say &quot;hello&quot;",
            "it&apos;s",
        ];
        let data = build_xlsx(strings);

        let result = XlsxConverter
            .convert(&data, &info_ext(".xlsx"), &MarkitOptions::default())
            .unwrap();
        assert!(result.markdown.contains("AT&T"), "AT&T");
        assert!(result.markdown.contains("x < y"), "x < y");
        assert!(result.markdown.contains("a > b"), "a > b");
        assert!(result.markdown.contains("say \"hello\""), "say \"hello\"");
        assert!(result.markdown.contains("it's"), "it's");
    }

    // -----------------------------------------------------------------------
    // Extra coverage
    // -----------------------------------------------------------------------

    #[test]
    fn test_missing_workbook_error() {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = ZipWriter::new(cursor);
        zip.start_file("dummy.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"dummy").unwrap();
        let data = zip.finish().unwrap().into_inner();

        let err = XlsxConverter
            .convert(&data, &info_ext(".xlsx"), &MarkitOptions::default())
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Invalid XLSX: missing workbook.xml"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_accepts_extension() {
        assert!(XlsxConverter.accepts(&StreamInfo {
            extension: Some(".xlsx".to_string()),
            ..Default::default()
        }));
    }

    #[test]
    fn test_accepts_mimetype() {
        assert!(XlsxConverter.accepts(&StreamInfo {
            mimetype: Some(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
            ),
            ..Default::default()
        }));
    }

    #[test]
    fn test_rejects_other() {
        assert!(!XlsxConverter.accepts(&StreamInfo {
            extension: Some(".csv".to_string()),
            ..Default::default()
        }));
        assert!(!XlsxConverter.accepts(&StreamInfo {
            mimetype: Some("text/plain".to_string()),
            ..Default::default()
        }));
    }

    #[test]
    fn test_basic_table_structure() {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();

        let ss_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"4\" uniqueCount=\"4\"><si><t>Name</t></si><si><t>Age</t></si><si><t>Alice</t></si><si><t>30</t></si></sst>";
        zip.start_file("xl/sharedStrings.xml", opts).unwrap();
        zip.write_all(ss_xml.as_bytes()).unwrap();

        let sheet_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\" t=\"s\"><v>1</v></c></row><row r=\"2\"><c r=\"A2\" t=\"s\"><v>2</v></c><c r=\"B2\" t=\"s\"><v>3</v></c></row></sheetData></worksheet>";
        zip.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
        zip.write_all(sheet_xml.as_bytes()).unwrap();

        let wb_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Sheet1\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>";
        zip.start_file("xl/workbook.xml", opts).unwrap();
        zip.write_all(wb_xml.as_bytes()).unwrap();

        let rels_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>";
        zip.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
        zip.write_all(rels_xml.as_bytes()).unwrap();

        let data = zip.finish().unwrap().into_inner();
        let result = XlsxConverter
            .convert(&data, &info_ext(".xlsx"), &MarkitOptions::default())
            .unwrap();

        let expected = "## Sheet1\n\n| Name | Age |\n| --- | --- |\n| Alice | 30 |";
        assert_eq!(result.markdown, expected);
    }

    #[test]
    fn test_boolean_cells() {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();

        let ss_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"0\" uniqueCount=\"0\"></sst>";
        zip.start_file("xl/sharedStrings.xml", opts).unwrap();
        zip.write_all(ss_xml.as_bytes()).unwrap();

        let sheet_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"b\"><v>1</v></c><c r=\"B1\" t=\"b\"><v>0</v></c></row><row r=\"2\"><c r=\"A2\"><v>42</v></c><c r=\"B2\"><v>3.14</v></c></row></sheetData></worksheet>";
        zip.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
        zip.write_all(sheet_xml.as_bytes()).unwrap();

        let wb_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Bools\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>";
        zip.start_file("xl/workbook.xml", opts).unwrap();
        zip.write_all(wb_xml.as_bytes()).unwrap();

        let rels_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>";
        zip.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
        zip.write_all(rels_xml.as_bytes()).unwrap();

        let data = zip.finish().unwrap().into_inner();
        let result = XlsxConverter
            .convert(&data, &info_ext(".xlsx"), &MarkitOptions::default())
            .unwrap();

        assert!(result.markdown.contains("TRUE"), "TRUE");
        assert!(result.markdown.contains("FALSE"), "FALSE");
        assert!(result.markdown.contains("42"), "42");
        assert!(result.markdown.contains("3.14"), "3.14");
    }
}
