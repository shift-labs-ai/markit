use anyhow::{anyhow, Result};
use std::io::{Cursor, Read};
use zip::ZipArchive;

use crate::types::{ConversionResult, Converter, StreamInfo};

const SF_NS: &str = "http://developer.apple.com/namespaces/sf";
const SFA_NS: &str = "http://developer.apple.com/namespaces/sfa";
const KEY_NS: &str = "http://developer.apple.com/namespaces/keynote2";

const IMAGE_EXTS: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".tiff", ".bmp"];

pub struct IWorkConverter;

impl Converter for IWorkConverter {
    fn name(&self) -> &'static str {
        "iwork"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        matches!(
            info.extension.as_deref(),
            Some(".pages") | Some(".key") | Some(".numbers")
        )
    }

    fn convert(&self, input: &[u8], info: &StreamInfo) -> Result<ConversionResult> {
        let cursor = Cursor::new(input);
        let mut archive = ZipArchive::new(cursor)?;

        match info.extension.as_deref() {
            Some(".pages") => convert_pages(&mut archive, info),
            Some(".key") => convert_keynote(&mut archive, info),
            Some(".numbers") => convert_numbers(&mut archive),
            Some(ext) => Err(anyhow!("Unsupported iWork format: {}", ext)),
            None => Err(anyhow!("Unsupported iWork format: ")),
        }
    }
}

// ---------------------------------------------------------------------------
// ZIP helpers
// ---------------------------------------------------------------------------

fn read_zip_entry(archive: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<String> {
    let mut file = archive
        .by_name(name)
        .map_err(|_| anyhow!("Invalid iWork file: missing {}", name))?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

fn file_names_in_archive(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Vec<String> {
    archive.file_names().map(|s| s.to_string()).collect()
}

fn read_zip_bytes(archive: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>> {
    let mut file = archive.by_name(name)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

fn is_image(name: &str) -> bool {
    let lower = name.to_lowercase();
    IMAGE_EXTS.iter().any(|ext| lower.ends_with(ext))
}

fn image_filename(name: &str, count: usize) -> String {
    let fallback = format!("image_{}", count);
    let base = name.rsplit('/').next().unwrap_or(&fallback);
    if base.is_empty() {
        fallback
    } else {
        base.to_string()
    }
}

// ---------------------------------------------------------------------------
// XML helpers
// ---------------------------------------------------------------------------

fn collect_text(node: roxmltree::Node) -> String {
    let mut result = String::new();
    for n in node.descendants() {
        if n.is_text() {
            result.push_str(n.text().unwrap_or(""));
        }
    }
    result
}

fn paragraph_prefix(style: &str) -> &'static str {
    let lower = style.to_lowercase();
    if lower.contains("title") {
        return "# ";
    }
    if lower.contains("subtitle") {
        return "## ";
    }
    if lower.contains("heading-1") || lower.contains("heading 1") {
        return "## ";
    }
    if lower.contains("heading-2") || lower.contains("heading 2") {
        return "### ";
    }
    if lower.contains("heading-3") || lower.contains("heading 3") {
        return "#### ";
    }
    if lower.contains("heading-4") || lower.contains("heading 4") {
        return "##### ";
    }
    if lower.contains("caption") {
        return "*";
    }
    ""
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

fn convert_pages(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    info: &StreamInfo,
) -> Result<ConversionResult> {
    let xml = read_zip_entry(archive, "index.xml")?;
    let doc = roxmltree::Document::parse(&xml)?;
    let root = doc.root_element();

    let image_dir = info.image_dir.as_deref();
    if let Some(dir) = image_dir {
        std::fs::create_dir_all(dir)?;
    }

    let mut lines: Vec<String> = Vec::new();
    let mut title: Option<String> = None;

    for node in root.descendants() {
        if !node.is_element() {
            continue;
        }
        let tag = node.tag_name();
        if tag.namespace() != Some(SF_NS) || tag.name() != "p" {
            continue;
        }

        let text = collect_text(node).trim().to_string();
        if text.is_empty() {
            continue;
        }

        let style = node.attribute((SF_NS, "style")).unwrap_or("");
        let prefix = paragraph_prefix(style);

        if title.is_none() {
            title = Some(text.clone());
        }
        lines.push(format!("{}{}", prefix, text));
    }

    // Extract images
    let names = file_names_in_archive(archive);
    let mut image_count = 0usize;
    for name in &names {
        if !is_image(name) || name.starts_with("QuickLook/") {
            continue;
        }
        image_count += 1;
        let img_name = image_filename(name, image_count);

        if let Some(dir) = image_dir {
            if let Ok(bytes) = read_zip_bytes(archive, name) {
                let filepath = format!("{}/{}", dir, img_name);
                std::fs::write(&filepath, &bytes)?;
                lines.push(format!("![{}]({})", img_name, filepath));
            }
        } else {
            lines.push(format!("<!-- image: {} -->", img_name));
        }
    }

    Ok(ConversionResult {
        markdown: lines.join("\n\n"),
        title,
    })
}

// ---------------------------------------------------------------------------
// Keynote
// ---------------------------------------------------------------------------

fn convert_keynote(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    info: &StreamInfo,
) -> Result<ConversionResult> {
    let xml = read_zip_entry(archive, "index.apxl")?;
    let doc = roxmltree::Document::parse(&xml)?;
    let root = doc.root_element();

    let image_dir = info.image_dir.as_deref();
    if let Some(dir) = image_dir {
        std::fs::create_dir_all(dir)?;
    }

    let mut sections: Vec<String> = Vec::new();
    let mut title: Option<String> = None;

    // Collect key:slide nodes and store their IDs so we can process them
    let slide_ids: Vec<roxmltree::NodeId> = root
        .descendants()
        .filter(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(KEY_NS)
                && n.tag_name().name() == "slide"
        })
        .map(|n| n.id())
        .collect();

    for (i, id) in slide_ids.iter().enumerate() {
        let slide = doc.get_node(*id).unwrap();
        let mut slide_lines: Vec<String> = vec![format!("<!-- Slide {} -->", i + 1)];
        let mut is_title_slot = true;

        for node in slide.descendants() {
            if !node.is_element() {
                continue;
            }
            let tag = node.tag_name();
            if tag.namespace() != Some(SF_NS) || tag.name() != "p" {
                continue;
            }
            let text = collect_text(node).trim().to_string();
            if text.is_empty() {
                continue;
            }
            if is_title_slot {
                slide_lines.push(format!("# {}", text));
                if title.is_none() {
                    title = Some(text);
                }
                is_title_slot = false;
            } else {
                slide_lines.push(text);
            }
        }

        sections.push(slide_lines.join("\n"));
    }

    // Extract images
    let names = file_names_in_archive(archive);
    let mut image_count = 0usize;
    for name in &names {
        if !is_image(name) || name.starts_with("QuickLook/") {
            continue;
        }
        image_count += 1;
        let img_name = image_filename(name, image_count);

        if let Some(dir) = image_dir {
            if let Ok(bytes) = read_zip_bytes(archive, name) {
                let filepath = format!("{}/{}", dir, img_name);
                std::fs::write(&filepath, &bytes)?;
                sections.push(format!("![{}]({})", img_name, filepath));
            }
        } else {
            sections.push(format!("<!-- image: {} -->", img_name));
        }
    }

    Ok(ConversionResult {
        markdown: sections.join("\n\n"),
        title,
    })
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

fn convert_numbers(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<ConversionResult> {
    let xml = read_zip_entry(archive, "index.xml")?;
    let doc = roxmltree::Document::parse(&xml)?;
    let root = doc.root_element();

    let grid_ids: Vec<roxmltree::NodeId> = root
        .descendants()
        .filter(|n| {
            n.is_element()
                && n.tag_name().namespace() == Some(SF_NS)
                && n.tag_name().name() == "grid"
        })
        .map(|n| n.id())
        .collect();

    if grid_ids.is_empty() {
        return convert_numbers_fallback(&doc);
    }

    let mut sections: Vec<String> = Vec::new();

    for id in &grid_ids {
        let grid = doc.get_node(*id).unwrap();
        let rows = extract_grid(grid);
        if rows.is_empty() {
            continue;
        }

        let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let mut rows = rows;
        for row in &mut rows {
            while row.len() < max_cols {
                row.push(String::new());
            }
        }

        let header = &rows[0];
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("| {} |", header.join(" | ")));
        lines.push(format!(
            "| {} |",
            header.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
        ));
        for row in &rows[1..] {
            lines.push(format!("| {} |", row.join(" | ")));
        }
        sections.push(lines.join("\n"));
    }

    Ok(ConversionResult::markdown(sections.join("\n\n")))
}

fn extract_grid(grid: roxmltree::Node) -> Vec<Vec<String>> {
    let datasource = grid.children().find(|n| {
        n.is_element()
            && n.tag_name().namespace() == Some(SF_NS)
            && n.tag_name().name() == "datasource"
    });

    let datasource = match datasource {
        Some(d) => d,
        None => return Vec::new(),
    };

    let num_cols = grid
        .attribute((SF_NS, "numcols"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut col_count = 0usize;
    let mut total_cells = 0usize;
    let mut all_values: Vec<String> = Vec::new();

    for child in datasource.children() {
        if !child.is_element() {
            continue;
        }
        let tag = child.tag_name();
        if tag.namespace() != Some(SF_NS) {
            continue;
        }

        let value = match tag.name() {
            "t" => {
                let ct = child.children().find(|n| {
                    n.is_element()
                        && n.tag_name().namespace() == Some(SF_NS)
                        && n.tag_name().name() == "ct"
                });
                match ct {
                    Some(ct_node) => ct_node.attribute((SFA_NS, "s")).unwrap_or("").to_string(),
                    None => collect_text(child).trim().to_string(),
                }
            }
            "n" => child.attribute((SF_NS, "v")).unwrap_or("").to_string(),
            "b" => {
                if child.attribute((SF_NS, "v")) == Some("1") {
                    "TRUE".to_string()
                } else {
                    "FALSE".to_string()
                }
            }
            "d" | "du" => child.attribute((SF_NS, "v")).unwrap_or("").to_string(),
            "e" => String::new(),
            _ => continue,
        };

        current_row.push(value.clone());
        all_values.push(value);
        col_count += 1;
        total_cells += 1;

        if num_cols > 0 && col_count >= num_cols {
            rows.push(std::mem::take(&mut current_row));
            col_count = 0;
        }
    }

    if !current_row.is_empty() {
        rows.push(current_row);
    }

    // Relayout fallback: only one row produced and fewer cells than numcols
    if rows.len() <= 1 && total_cells > 0 && total_cells < num_cols {
        let cols = if total_cells.is_multiple_of(2) { 2 } else { 1 };
        let mut relaid: Vec<Vec<String>> = Vec::new();
        let mut i = 0;
        while i < all_values.len() {
            let end = (i + cols).min(all_values.len());
            relaid.push(all_values[i..end].to_vec());
            i += cols;
        }
        return relaid;
    }

    rows
}

fn convert_numbers_fallback(doc: &roxmltree::Document) -> Result<ConversionResult> {
    let root = doc.root_element();
    let mut values: Vec<String> = Vec::new();

    for node in root.descendants() {
        if !node.is_element() {
            continue;
        }
        let tag = node.tag_name();
        if tag.namespace() == Some(SF_NS) && tag.name() == "t" {
            let ct = node.children().find(|n| {
                n.is_element()
                    && n.tag_name().namespace() == Some(SF_NS)
                    && n.tag_name().name() == "ct"
            });
            if let Some(val) = ct.and_then(|n| n.attribute((SFA_NS, "s"))) {
                if !val.is_empty() {
                    values.push(val.to_string());
                }
            }
        } else if tag.namespace() == Some(SF_NS) && tag.name() == "n" {
            if let Some(val) = node.attribute((SF_NS, "v")) {
                if !val.is_empty() {
                    values.push(val.to_string());
                }
            }
        }
    }

    Ok(ConversionResult::markdown(values.join("\n")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Converter, StreamInfo};
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn info(ext: &str) -> StreamInfo {
        StreamInfo {
            extension: Some(ext.to_string()),
            ..Default::default()
        }
    }

    fn build_pages(style: Option<&str>, include_image: bool) -> Vec<u8> {
        let style = style.unwrap_or("text-20-paragraphstyle-Body 1");
        let xml = format!(
            r#"<?xml version="1.0"?>
<sl:document xmlns:sfa="http://developer.apple.com/namespaces/sfa"
             xmlns:sf="http://developer.apple.com/namespaces/sf"
             xmlns:sl="http://developer.apple.com/namespaces/sl">
  <sf:section>
    <sf:layout>
      <sf:p sf:style="{style}">Hello from Pages</sf:p>
    </sf:layout>
  </sf:section>
</sl:document>"#
        );
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();
        zip.start_file("index.xml", opts).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        if include_image {
            // 1x1 red PNG (minimal)
            let png_data: &[u8] = &[
                0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
                0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // IHDR length + type
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
                0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, // 8-bit RGB
                0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf,
                0xc0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00,
                0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
            ];
            zip.start_file("media/photo.png", opts).unwrap();
            zip.write_all(png_data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn build_keynote() -> Vec<u8> {
        let xml = r#"<?xml version="1.0"?>
<key:presentation xmlns:sfa="http://developer.apple.com/namespaces/sfa"
                  xmlns:sf="http://developer.apple.com/namespaces/sf"
                  xmlns:key="http://developer.apple.com/namespaces/keynote2">
  <key:slide-list>
    <key:slide>
      <key:title-placeholder>
        <sf:p>Slide Title</sf:p>
      </key:title-placeholder>
      <key:body-placeholder>
        <sf:p>Body content here</sf:p>
      </key:body-placeholder>
    </key:slide>
    <key:slide>
      <key:title-placeholder>
        <sf:p>Second Slide</sf:p>
      </key:title-placeholder>
    </key:slide>
  </key:slide-list>
</key:presentation>"#;
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();
        zip.start_file("index.apxl", opts).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    fn build_numbers(numcols: usize, cells: &str) -> Vec<u8> {
        let xml = format!(
            r#"<?xml version="1.0"?>
<sl:document xmlns:sfa="http://developer.apple.com/namespaces/sfa"
             xmlns:sf="http://developer.apple.com/namespaces/sf"
             xmlns:sl="http://developer.apple.com/namespaces/sl">
  <sf:grid sf:numcols="{numcols}" sf:numrows="10">
    <sf:datasource>{cells}</sf:datasource>
  </sf:grid>
</sl:document>"#
        );
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();
        zip.start_file("index.xml", opts).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    // --- Pages ---

    #[test]
    fn pages_extracts_body_text() {
        let bytes = build_pages(None, false);
        let result = IWorkConverter.convert(&bytes, &info(".pages")).unwrap();
        assert!(
            result.markdown.contains("Hello from Pages"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn pages_detects_title_style() {
        let bytes = build_pages(Some("text-0-paragraphstyle-Title"), false);
        let result = IWorkConverter.convert(&bytes, &info(".pages")).unwrap();
        assert!(
            result.markdown.contains("# Hello from Pages"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn pages_detects_heading_style() {
        let bytes = build_pages(Some("text-11-paragraphstyle-Heading 1"), false);
        let result = IWorkConverter.convert(&bytes, &info(".pages")).unwrap();
        assert!(
            result.markdown.contains("## Hello from Pages"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn pages_image_placeholder() {
        let bytes = build_pages(None, true);
        let result = IWorkConverter.convert(&bytes, &info(".pages")).unwrap();
        assert!(
            result.markdown.contains("<!-- image: photo.png -->"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn pages_image_to_dir() {
        let dir = std::env::temp_dir().join(format!("markit-pages-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir_path = dir.to_str().unwrap().to_string();

        let bytes = build_pages(None, true);
        let mut si = info(".pages");
        si.image_dir = Some(dir_path.clone());
        let result = IWorkConverter.convert(&bytes, &si).unwrap();

        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            result.markdown.contains("![photo.png]"),
            "got: {}",
            result.markdown
        );
    }

    // --- Keynote ---

    #[test]
    fn keynote_slide_text() {
        let bytes = build_keynote();
        let result = IWorkConverter.convert(&bytes, &info(".key")).unwrap();
        assert!(
            result.markdown.contains("# Slide Title"),
            "got: {}",
            result.markdown
        );
        assert!(
            result.markdown.contains("Body content here"),
            "got: {}",
            result.markdown
        );
        assert!(
            result.markdown.contains("<!-- Slide 1 -->"),
            "got: {}",
            result.markdown
        );
        assert!(
            result.markdown.contains("<!-- Slide 2 -->"),
            "got: {}",
            result.markdown
        );
        assert!(
            result.markdown.contains("# Second Slide"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn keynote_title_from_first_slide() {
        let bytes = build_keynote();
        let result = IWorkConverter.convert(&bytes, &info(".key")).unwrap();
        assert_eq!(result.title.as_deref(), Some("Slide Title"));
    }

    // --- Numbers ---

    #[test]
    fn numbers_extracts_table() {
        let cells = r#"
      <sf:t sf:ct="0" sf:f="0" sf:s="s1"><sf:ct sfa:s="Name"/></sf:t>
      <sf:t sf:f="0" sf:s="s1"><sf:ct sfa:s="Score"/></sf:t>
      <sf:t sf:ct="255" sf:f="0" sf:s="s2"><sf:ct sfa:s="Alice"/></sf:t>
      <sf:n sf:f="0" sf:s="s3" sf:v="95"/>
      <sf:t sf:ct="255" sf:f="0" sf:s="s2"><sf:ct sfa:s="Bob"/></sf:t>
      <sf:n sf:f="0" sf:s="s3" sf:v="87"/>"#;
        let bytes = build_numbers(2, cells);
        let result = IWorkConverter.convert(&bytes, &info(".numbers")).unwrap();
        assert!(
            result.markdown.contains("| Name | Score |"),
            "got: {}",
            result.markdown
        );
        assert!(
            result.markdown.contains("| Alice | 95 |"),
            "got: {}",
            result.markdown
        );
        assert!(
            result.markdown.contains("| Bob | 87 |"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn numbers_oversized_grid_relayout() {
        let cells = r#"
      <sf:t sf:ct="0" sf:f="0" sf:s="s1"><sf:ct sfa:s="Name"/></sf:t>
      <sf:t sf:f="0" sf:s="s1"><sf:ct sfa:s="Score"/></sf:t>
      <sf:t sf:ct="255" sf:f="0" sf:s="s2"><sf:ct sfa:s="Alice"/></sf:t>
      <sf:n sf:f="0" sf:s="s3" sf:v="95"/>"#;
        // numcols=7 but only 4 cells — relayouts to 2 cols
        let bytes = build_numbers(7, cells);
        let result = IWorkConverter.convert(&bytes, &info(".numbers")).unwrap();
        assert!(
            result.markdown.contains("| Name | Score |"),
            "got: {}",
            result.markdown
        );
        assert!(
            result.markdown.contains("| Alice | 95 |"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn accepts_iwork_extensions() {
        for ext in &[".pages", ".key", ".numbers"] {
            assert!(IWorkConverter.accepts(&info(ext)), "should accept {}", ext);
        }
        assert!(!IWorkConverter.accepts(&info(".pdf")));
    }

    #[test]
    fn missing_index_xml_errors() {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let zip = ZipWriter::new(cursor);
        let bytes = zip.finish().unwrap().into_inner();
        let err = IWorkConverter.convert(&bytes, &info(".pages")).unwrap_err();
        assert!(
            err.to_string().contains("Invalid iWork file"),
            "got: {}",
            err
        );
    }
}
