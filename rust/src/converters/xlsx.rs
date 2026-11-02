use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::io::{Cursor, Read, Seek};
use zip::ZipArchive;

use crate::types::{ConversionResult, Converter, StreamInfo};

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

    fn convert(&self, input: &[u8], _info: &StreamInfo) -> Result<ConversionResult> {
        let cursor = Cursor::new(input);
        let mut archive = ZipArchive::new(cursor)?;

        // Parse shared strings
        let shared = match read_zip_str(&mut archive, "xl/sharedStrings.xml")? {
            Some(xml) => parse_shared_strings(&xml)?,
            None => Vec::new(),
        };

        let wb_xml = read_zip_str(&mut archive, "xl/workbook.xml")?
            .ok_or_else(|| anyhow!("Invalid XLSX: missing workbook.xml"))?;
        let (sheets, date1904) = parse_workbook(&wb_xml)?;

        let formats = CellFormats {
            temporal: match read_zip_str(&mut archive, "xl/styles.xml")? {
                Some(xml) => parse_temporal_styles(&xml)?,
                None => Vec::new(),
            },
            date1904,
        };

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

            let mut table_rows = parse_sheet_rows(&sheet_xml, &shared, &formats)?;
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

/// Parse xl/workbook.xml -> ([(sheet_name, r_id)], uses the 1904 date system).
fn parse_workbook(xml: &str) -> Result<(Vec<(String, String)>, bool)> {
    let doc = roxmltree::Document::parse(xml)?;
    let mut sheets = Vec::new();
    let mut date1904 = false;
    for node in doc.root_element().descendants().filter(|n| n.is_element()) {
        match node.tag_name().name() {
            "sheet" => {
                let name = node.attribute("name").unwrap_or("").to_string();
                // r:id is a namespaced attribute
                let r_id = node.attribute((REL_NS, "id")).unwrap_or("").to_string();
                if !name.is_empty() && !r_id.is_empty() {
                    sheets.push((name, r_id));
                }
            }
            "workbookPr" => {
                date1904 = matches!(node.attribute("date1904"), Some("1" | "true"));
            }
            _ => {}
        }
    }
    Ok((sheets, date1904))
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

// ---------------------------------------------------------------------------
// Dates and times
// ---------------------------------------------------------------------------

/// The calendar and clock parts a date/time number format displays.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Temporal {
    date: bool,
    time: bool,
    seconds: bool,
}

/// What the workbook says about turning a numeric cell into a date.
struct CellFormats {
    /// Indexed by a cell's `s` attribute (its `cellXfs` entry); `None`
    /// for every style that is not a date or time.
    temporal: Vec<Option<Temporal>>,
    /// Serials count days from 1904-01-01 instead of 1900-01-00.
    date1904: bool,
}

/// Parse xl/styles.xml into the date/time meaning of each `cellXfs` entry.
fn parse_temporal_styles(xml: &str) -> Result<Vec<Option<Temporal>>> {
    let doc = roxmltree::Document::parse(xml)?;
    let element = |name: &'static str| {
        doc.descendants()
            .find(move |n| n.is_element() && n.tag_name().name() == name)
    };
    let custom: HashMap<u32, &str> = element("numFmts")
        .into_iter()
        .flat_map(|n| n.children())
        .filter(|n| n.is_element() && n.tag_name().name() == "numFmt")
        .filter_map(|n| {
            let id = n.attribute("numFmtId")?.parse().ok()?;
            Some((id, n.attribute("formatCode")?))
        })
        .collect();
    Ok(element("cellXfs")
        .into_iter()
        .flat_map(|n| n.children())
        .filter(|n| n.is_element() && n.tag_name().name() == "xf")
        .map(|xf| {
            let id: u32 = xf
                .attribute("numFmtId")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            match custom.get(&id) {
                Some(code) => temporal_format(code),
                None => builtin_temporal_format(id),
            }
        })
        .collect())
}

/// The date/time formats among Excel's built-in number formats, which
/// workbooks reference by id alone. 46 (`[h]:mm:ss`) is an elapsed
/// duration, not a time of day.
fn builtin_temporal_format(id: u32) -> Option<Temporal> {
    let (date, time, seconds) = match id {
        14..=17 => (true, false, false),
        18 | 20 => (false, true, false),
        19 | 21 | 45 | 47 => (false, true, true),
        22 => (true, true, false),
        _ => return None,
    };
    Some(Temporal {
        date,
        time,
        seconds,
    })
}

/// Classify a custom format code by the date and time tokens of the
/// section that formats positive numbers. Quoted literals, escaped
/// characters, fill/padding characters and bracketed colours or locales
/// carry no tokens; an elapsed-time bracket (`[h]`, `[mm]`, `[ss]`) makes
/// the format a duration, which stays a number.
fn temporal_format(code: &str) -> Option<Temporal> {
    let mut tokens = String::new();
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            ';' => break,
            '"' => {
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                let bracket: String = chars.by_ref().take_while(|&b| b != ']').collect();
                let lower = bracket.to_ascii_lowercase();
                if !lower.is_empty() && lower.chars().all(|b| matches!(b, 'h' | 'm' | 's')) {
                    return None;
                }
            }
            _ => tokens.push(c.to_ascii_lowercase()),
        }
    }
    let date = tokens.contains(['y', 'd']);
    let time = tokens.contains(['h', 's']);
    // A lone `m` is a month (`mmmm`); beside an hour or second it is a minute.
    let month_only = !date && !time && tokens.contains('m');
    (date || time || month_only).then_some(Temporal {
        date: date || month_only,
        time,
        seconds: tokens.contains('s'),
    })
}

/// Render a date/time serial as ISO 8601 (`2014-01-01`, `14:30`,
/// `2014-01-01 14:30:05`). `None` where the serial names no real
/// instant: negative, or one of the 1900 system's fictitious days,
/// 1900-01-00 and 1900-02-29.
fn render_serial(serial: f64, format: Temporal, date1904: bool) -> Option<String> {
    if !serial.is_finite() || serial < 0.0 {
        return None;
    }
    let total_seconds = (serial * 86_400.0).round() as i64;
    let (days, seconds) = (total_seconds / 86_400, total_seconds % 86_400);
    let clock = if format.seconds {
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 3600, seconds / 60 % 60)
    };
    if !format.date {
        return Some(clock);
    }
    // Days since 1970-01-01 of serial day 0. The 1900 system counts a
    // 29 February 1900 that never existed (serial 60), so serials before
    // it run one day later than those after it.
    let epoch = match (date1904, days) {
        (true, _) => -24_107,
        (false, 0 | 60) => return None,
        (false, ..60) => -25_568,
        (false, _) => -25_569,
    };
    let (y, m, d) = civil_from_days(epoch + days);
    let date = format!("{y:04}-{m:02}-{d:02}");
    Some(if format.time {
        format!("{date} {clock}")
    } else {
        date
    })
}

/// Proleptic Gregorian (year, month, day) for a count of days since
/// 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Parse a worksheet XML and return rows as Vec<Vec<String>>.
///
/// Cells are appended in document order; cell references (`r="A1"`) are
/// not consulted, so a skipped column closes up.
fn parse_sheet_rows(
    xml: &str,
    shared: &[String],
    formats: &CellFormats,
) -> Result<Vec<Vec<String>>> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let mut rows = Vec::new();
    for node in root.descendants() {
        if node.is_element() && node.tag_name().name() == "row" {
            let cells: Vec<String> = node
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == "c")
                .map(|c| get_cell_value(c, shared, formats))
                .collect();
            if !cells.is_empty() {
                rows.push(cells);
            }
        }
    }
    Ok(rows)
}

/// The text of a cell's stored value, its `<v>` child.
fn stored_value(c: roxmltree::Node) -> String {
    c.children()
        .find(|n| n.is_element() && n.tag_name().name() == "v")
        .map(text_content)
        .unwrap_or_default()
}

/// Derive the string value of a <c> element.
fn get_cell_value(c: roxmltree::Node, shared: &[String], formats: &CellFormats) -> String {
    let t = c.attribute("t").unwrap_or("");

    match t {
        "s" => {
            // Shared string: <v> contains the 0-based index
            let idx: usize = stored_value(c).trim().parse().unwrap_or(usize::MAX);
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
            if stored_value(c).trim() == "1" {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }
        }
        _ => {
            // Number, formula result, error, etc. -- <v> as raw string,
            // except a number whose style is a date or time.
            let raw = stored_value(c);
            let temporal = matches!(t, "" | "n")
                .then(|| c.attribute("s")?.parse::<usize>().ok())
                .flatten()
                .and_then(|s| formats.temporal.get(s).copied().flatten());
            temporal
                .and_then(|format| {
                    render_serial(raw.trim().parse().ok()?, format, formats.date1904)
                })
                .unwrap_or(raw)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StreamInfo;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn info_ext(ext: &str) -> StreamInfo {
        StreamInfo {
            extension: Some(ext.to_string()),
            ..Default::default()
        }
    }

    /// One row holding one shared-string cell per entry, in order.
    fn shared_string_row(shared_strings: &[&str]) -> String {
        let cells: String = (0..shared_strings.len())
            .map(|i| format!("<c t=\"s\"><v>{i}</v></c>"))
            .collect();
        Workbook {
            shared_strings,
            rows: &format!("<row>{cells}</row>"),
            ..Default::default()
        }
        .convert()
    }

    #[test]
    fn test_entity_references_large() {
        // Thousands of entity references must not trip a parser limit.
        let strings: Vec<String> = (0..2000).map(|i| format!("val&amp;ue_{i}")).collect();
        let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
        let markdown = shared_string_row(&refs);
        for expected in ["val&ue_0", "val&ue_999", "val&ue_1999"] {
            assert!(markdown.contains(expected), "{expected}");
        }
    }

    #[test]
    fn test_xml_entities() {
        let markdown = shared_string_row(&[
            "AT&amp;T",
            "x &lt; y",
            "a &gt; b",
            "say &quot;hello&quot;",
            "it&apos;s",
        ]);
        for expected in ["AT&T", "x < y", "a > b", "say \"hello\"", "it's"] {
            assert!(markdown.contains(expected), "{expected}");
        }
    }

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
            .convert(&data, &info_ext(".xlsx"))
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

    /// Parts of a one-sheet workbook. Shared strings and styles are
    /// optional parts; `workbook_pr` is the `<workbookPr>` element or "".
    #[derive(Default)]
    struct Workbook<'a> {
        shared_strings: &'a [&'a str],
        rows: &'a str,
        styles: Option<&'a str>,
        workbook_pr: &'a str,
    }

    impl Workbook<'_> {
        fn convert(&self) -> String {
            const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
            let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
            let mut part = |name: &str, xml: String| {
                zip.start_file(name, SimpleFileOptions::default()).unwrap();
                zip.write_all(xml.as_bytes()).unwrap();
            };
            if !self.shared_strings.is_empty() {
                let si: String = self
                    .shared_strings
                    .iter()
                    .map(|s| format!("<si><t>{s}</t></si>"))
                    .collect();
                part(
                    "xl/sharedStrings.xml",
                    format!("<sst xmlns=\"{MAIN}\">{si}</sst>"),
                );
            }
            if let Some(styles) = self.styles {
                part(
                    "xl/styles.xml",
                    format!("<styleSheet xmlns=\"{MAIN}\">{styles}</styleSheet>"),
                );
            }
            part(
                "xl/worksheets/sheet1.xml",
                format!(
                    "<worksheet xmlns=\"{MAIN}\"><sheetData>{}</sheetData></worksheet>",
                    self.rows
                ),
            );
            part(
                "xl/workbook.xml",
                format!(
                    "<workbook xmlns=\"{MAIN}\" xmlns:r=\"{REL_NS}\">{}<sheets>\
                     <sheet name=\"Sheet1\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>",
                    self.workbook_pr
                ),
            );
            part(
                "xl/_rels/workbook.xml.rels",
                "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
                 <Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\" \
                 Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\"/>\
                 </Relationships>"
                    .to_string(),
            );
            let data = zip.finish().unwrap().into_inner();
            XlsxConverter
                .convert(&data, &info_ext(".xlsx"))
                .unwrap()
                .markdown
        }
    }

    #[test]
    fn test_basic_table_structure() {
        let markdown = Workbook {
            shared_strings: &["Name", "Age", "Alice", "30"],
            rows: "<row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\" t=\"s\"><v>1</v></c></row>\
                   <row r=\"2\"><c r=\"A2\" t=\"s\"><v>2</v></c><c r=\"B2\" t=\"s\"><v>3</v></c></row>",
            ..Default::default()
        }
        .convert();
        assert_eq!(
            markdown,
            "## Sheet1\n\n| Name | Age |\n| --- | --- |\n| Alice | 30 |"
        );
    }

    #[test]
    fn test_boolean_cells() {
        let markdown = Workbook {
            rows: "<row r=\"1\"><c r=\"A1\" t=\"b\"><v>1</v></c><c r=\"B1\" t=\"b\"><v>0</v></c></row>\
                   <row r=\"2\"><c r=\"A2\"><v>42</v></c><c r=\"B2\"><v>3.14</v></c></row>",
            ..Default::default()
        }
        .convert();
        assert_eq!(
            markdown,
            "## Sheet1\n\n| TRUE | FALSE |\n| --- | --- |\n| 42 | 3.14 |"
        );
    }

    // -----------------------------------------------------------------------
    // Dates and times
    // -----------------------------------------------------------------------

    /// One cell per (style index, stored value), on a header row of `x`.
    fn styled_cells(styles: &str, workbook_pr: &str, cells: &[(usize, &str)]) -> Vec<String> {
        let row: String = cells
            .iter()
            .map(|(s, v)| format!("<c s=\"{s}\"><v>{v}</v></c>"))
            .collect();
        let header: String = cells.iter().map(|_| "<c t=\"s\"><v>0</v></c>").collect();
        let markdown = Workbook {
            shared_strings: &["x"],
            rows: &format!("<row>{header}</row><row>{row}</row>"),
            styles: Some(styles),
            workbook_pr,
        }
        .convert();
        let last = markdown.lines().last().unwrap();
        last.trim_matches(['|', ' '])
            .split(" | ")
            .map(str::to_string)
            .collect()
    }

    /// `cellXfs` entries in order, one per `numFmtId`, after any custom
    /// `numFmts`.
    fn stylesheet(custom: &[(u32, &str)], xf_ids: &[u32]) -> String {
        let fmts: String = custom
            .iter()
            .map(|(id, code)| format!("<numFmt numFmtId=\"{id}\" formatCode=\"{code}\"/>"))
            .collect();
        let xfs: String = xf_ids
            .iter()
            .map(|id| format!("<xf numFmtId=\"{id}\"/>"))
            .collect();
        format!("<numFmts>{fmts}</numFmts><cellXfs>{xfs}</cellXfs>")
    }

    #[test]
    fn builtin_date_and_time_formats_render_as_iso() {
        // 0 General, 14 m/d/yyyy, 22 m/d/yy h:mm, 20 h:mm, 21 h:mm:ss.
        let styles = stylesheet(&[], &[0, 14, 22, 20, 21]);
        assert_eq!(
            styled_cells(
                &styles,
                "",
                &[
                    (0, "41640"),
                    (1, "41640"),
                    (2, "46257.6041666667"),
                    (3, "0.378472222222222"),
                    (4, "0.378530092592593"),
                ]
            ),
            [
                "41640",
                "2014-01-01",
                "2026-08-23 14:30",
                "09:05",
                "09:05:05"
            ]
        );
    }

    #[test]
    fn custom_formats_are_read_by_their_tokens() {
        let styles = stylesheet(
            &[
                (164, "m/d/yy\\ h:mm;@"),
                (165, "[$-409]mmmm\\ d\\,\\ yyyy"),
                (166, "[h]:mm"),
                (167, "0 &quot;days&quot;"),
                (168, "[Red]#,##0.00"),
            ],
            &[164, 165, 166, 167, 168],
        );
        assert_eq!(
            styled_cells(
                &styles,
                "",
                &[
                    (0, "41640.5"),
                    (1, "41640"),
                    (2, "1.5"),
                    (3, "12"),
                    (4, "-3.5"),
                ]
            ),
            // An elapsed duration, quoted literals and a colour are numbers.
            ["2014-01-01 12:00", "2014-01-01", "1.5", "12", "-3.5"]
        );
    }

    #[test]
    fn currency_and_percent_keep_their_stored_value() {
        // 7 and 10 are the built-in currency and percent formats.
        let styles = stylesheet(&[], &[7, 10]);
        assert_eq!(
            styled_cells(&styles, "", &[(0, "1234.5"), (1, "0.157")]),
            ["1234.5", "0.157"]
        );
    }

    #[test]
    fn the_1900_system_skips_its_fictitious_days() {
        let styles = stylesheet(&[], &[14]);
        assert_eq!(
            styled_cells(
                &styles,
                "",
                &[(0, "0.5"), (0, "1"), (0, "59"), (0, "60"), (0, "61")]
            ),
            ["0.5", "1900-01-01", "1900-02-28", "60", "1900-03-01"]
        );
    }

    #[test]
    fn the_1904_system_counts_from_1904() {
        let styles = stylesheet(&[], &[14]);
        assert_eq!(
            styled_cells(
                &styles,
                "<workbookPr date1904=\"1\"/>",
                &[(0, "0"), (0, "40178")]
            ),
            ["1904-01-01", "2014-01-01"]
        );
    }

    #[test]
    fn time_rounding_carries_into_the_next_day() {
        // 23:59:59.6 rounds to midnight of the following day.
        let styles = stylesheet(&[], &[22]);
        assert_eq!(
            styled_cells(&styles, "", &[(0, "41640.99999537")]),
            ["2014-01-02 00:00"]
        );
    }
}
