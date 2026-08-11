//! DOCX → Markdown converter.
//!
//! Pipeline:
//!   ZIP → word/document.xml (+ rels, numbering, images)
//!       → HTML string (mammoth-style)
//!       → html_to_md engine
//!
//! Supported: headings, bold/italic/strikethrough, tables, bullet/ordered lists,
//! hyperlinks, inline images (write to disk or emit placeholder comment).

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};

use crate::converters::ooxml::{
    office_document_path, parse_relationships, relationship_part_path, resolve_part_path,
};
use crate::types::{ConversionResult, Converter, StreamInfo};
use crate::utils::html_to_md::{html_to_markdown_generated, normalize_tables_html};

// ── OOXML namespace URIs ──────────────────────────────────────────────────────
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
// Placeholder token embedded in plain text so the HTML stripper won't eat it.
const IMG_TOKEN_PREFIX: &str = "MARKITIMGTOKEN";

// ── Public converter ──────────────────────────────────────────────────────────

pub struct DocxConverter;

impl Converter for DocxConverter {
    fn name(&self) -> &'static str {
        "docx"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if info.extension.as_deref() == Some(".docx") {
            return true;
        }
        info.mimetype.as_deref().is_some_and(|m| {
            m.starts_with("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        })
    }

    fn convert(&self, input: &[u8], info: &StreamInfo) -> Result<ConversionResult> {
        let cursor = Cursor::new(input);
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|e| anyhow!("Invalid DOCX: {}", e))?;

        let document_path = read_zip_str(&mut archive, "_rels/.rels")
            .ok()
            .and_then(|xml| office_document_path(&xml).ok())
            .unwrap_or_else(|| "word/document.xml".to_string());

        // Relationship map: id → (rel_type, target)
        let rels = read_rels(&mut archive, &document_path).unwrap_or_default();

        let doc_xml = read_zip_str(&mut archive, &document_path)
            .map_err(|_| anyhow!("Invalid DOCX: missing main document part {document_path}"))?;

        // Numbering: numId → isOrdered (optional)
        let num_types = read_numbering(&mut archive, &rels, &document_path).unwrap_or_default();

        // Image blobs: rel_id → (bytes, extension)
        let images = load_images(&mut archive, &rels, &document_path);

        // Prepare image output dir
        let image_dir = info.image_dir.as_deref();
        if let Some(dir) = image_dir {
            fs::create_dir_all(dir)?;
        }

        // Convert to HTML
        let mut image_count: usize = 0;
        let html = doc_to_html(
            &doc_xml,
            &rels,
            &images,
            &num_types,
            image_dir,
            &mut image_count,
        )?;

        let normalized = normalize_tables_html(&html);
        // Generated HTML cannot contain <noscript> (fixed tag set,
        // entity-escaped text), so the noscript-rewrite scans are skipped.
        let mut markdown = html_to_markdown_generated(&normalized);

        // Replace image tokens with placeholder comments (no-imageDir path).
        // The HTML engine may escape underscores (MARKIT_IMG → MARKIT\_IMG),
        // so also try escaped variants. With MARKITIMGTOKEN (no underscores) this
        // is a non-issue, but kept for safety.
        if image_dir.is_none() {
            for i in 1..=image_count {
                let token = format!("{}{}", IMG_TOKEN_PREFIX, i);
                let comment = format!("<!-- image: image_{} -->", i);
                let escaped = token.replace('_', "\\_");
                markdown = markdown
                    .replace(&token, &comment)
                    .replace(&escaped, &comment);
            }
        }

        Ok(ConversionResult::markdown(markdown.trim()))
    }
}

// ── ZIP helpers ───────────────────────────────────────────────────────────────

fn read_zip_bytes(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>> {
    let mut file = archive.by_name(name)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

fn read_zip_str(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<String> {
    let bytes = read_zip_bytes(archive, name)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// ── Relationship parsing ──────────────────────────────────────────────────────

/// Returns id → (rel_type, target)
fn read_rels(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    document_path: &str,
) -> Result<HashMap<String, (String, String)>> {
    let xml = read_zip_str(archive, &relationship_part_path(document_path))?;
    Ok(parse_relationships(&xml))
}

// ── Numbering (list type detection) ──────────────────────────────────────────

/// Returns numId → true(ordered) / false(unordered).
fn read_numbering(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    rels: &HashMap<String, (String, String)>,
    document_path: &str,
) -> Result<HashMap<String, bool>> {
    let numbering_path = rels
        .values()
        .find(|(kind, _)| kind.ends_with("/numbering"))
        .and_then(|(_, target)| resolve_part_path(document_path, target))
        .unwrap_or_else(|| {
            resolve_part_path(document_path, "numbering.xml")
                .unwrap_or_else(|| "word/numbering.xml".to_string())
        });
    let xml = match read_zip_str(archive, &numbering_path) {
        Ok(s) => s,
        Err(_) => return Ok(HashMap::new()),
    };
    let doc = roxmltree::Document::parse(&xml)?;
    let root = doc.root_element();

    // Build abstractNumId → isOrdered
    let mut abstract_ordered: HashMap<String, bool> = HashMap::new();
    for node in root.children().filter(|n| n.is_element()) {
        if is_wn(node, "abstractNum") {
            let abs_id = wattr(node, "abstractNumId").unwrap_or("").to_string();
            let ordered = node
                .descendants()
                .find(|n| is_wn(*n, "numFmt"))
                .and_then(|n| wattr(n, "val"))
                .map(|v| !matches!(v, "bullet" | "none"))
                .unwrap_or(false);
            abstract_ordered.insert(abs_id, ordered);
        }
    }

    // Build numId → isOrdered via abstractNumId reference
    let mut map: HashMap<String, bool> = HashMap::new();
    for node in root.children().filter(|n| n.is_element()) {
        if is_wn(node, "num") {
            let num_id = wattr(node, "numId").unwrap_or("").to_string();
            let abs_id = node
                .descendants()
                .find(|n| is_wn(*n, "abstractNumId"))
                .and_then(|n| wattr(n, "val"))
                .unwrap_or("")
                .to_string();
            let ordered = *abstract_ordered.get(&abs_id).unwrap_or(&false);
            map.insert(num_id, ordered);
        }
    }
    Ok(map)
}

// ── Image loading ─────────────────────────────────────────────────────────────

/// Returns rel_id → (bytes, extension)
fn load_images(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    rels: &HashMap<String, (String, String)>,
    document_path: &str,
) -> HashMap<String, (Vec<u8>, String)> {
    let mut map = HashMap::new();
    for (rel_id, (rel_type, target)) in rels {
        if !rel_type.contains("image") {
            continue;
        }
        let Some(zip_path) = resolve_part_path(document_path, target) else {
            continue;
        };
        let ext = zip_path.rsplit('.').next().unwrap_or("png").to_lowercase();
        if let Ok(bytes) = read_zip_bytes(archive, &zip_path) {
            map.insert(rel_id.clone(), (bytes, ext));
        }
    }
    map
}

// ── Main HTML generation ──────────────────────────────────────────────────────

fn doc_to_html(
    doc_xml: &str,
    rels: &HashMap<String, (String, String)>,
    images: &HashMap<String, (Vec<u8>, String)>,
    num_types: &HashMap<String, bool>,
    image_dir: Option<&str>,
    image_count: &mut usize,
) -> Result<String> {
    let doc = roxmltree::Document::parse(doc_xml)?;
    let root = doc.root_element();

    let body = root
        .descendants()
        .find(|n| is_wn(*n, "body"))
        .ok_or_else(|| anyhow!("Invalid DOCX: missing w:body"))?;

    let mut html = String::new();
    let mut list_stack: Vec<(String, u32, bool)> = Vec::new();
    // One scratch buffer reused for every paragraph's inner HTML.
    let mut scratch = String::new();

    for node in body.children().filter(|n| n.is_element()) {
        if is_wn(node, "p") {
            emit_paragraph(
                node,
                rels,
                images,
                num_types,
                image_dir,
                image_count,
                &mut html,
                &mut list_stack,
                &mut scratch,
            );
        } else if is_wn(node, "tbl") {
            close_all_lists(&mut list_stack, &mut html);
            emit_table(
                node,
                rels,
                images,
                image_dir,
                image_count,
                &mut html,
                &mut scratch,
            );
        }
    }

    close_all_lists(&mut list_stack, &mut html);
    Ok(html)
}

// ── Paragraph ─────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn emit_paragraph(
    para: roxmltree::Node,
    rels: &HashMap<String, (String, String)>,
    images: &HashMap<String, (Vec<u8>, String)>,
    num_types: &HashMap<String, bool>,
    image_dir: Option<&str>,
    image_count: &mut usize,
    html: &mut String,
    list_stack: &mut Vec<(String, u32, bool)>,
    scratch: &mut String,
) {
    let ppr = find_wchild(para, "pPr");

    let style_val = ppr
        .and_then(|p| find_wchild(p, "pStyle"))
        .and_then(|s| wattr(s, "val"))
        .unwrap_or("");

    let tag = style_to_tag(style_val);

    let num_pr = ppr.and_then(|p| find_wchild(p, "numPr"));
    let num_id = num_pr
        .and_then(|np| find_wchild(np, "numId"))
        .and_then(|n| wattr(n, "val"))
        .filter(|v| *v != "0")
        .map(|v| v.to_string());
    let ilvl = num_pr
        .and_then(|np| find_wchild(np, "ilvl"))
        .and_then(|n| wattr(n, "val"))
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);

    scratch.clear();
    para_content_html(para, rels, images, image_dir, image_count, scratch);
    let inner = scratch.as_str();

    if let Some(ref nid) = num_id {
        let is_ordered = *num_types.get(nid.as_str()).unwrap_or(&false);
        manage_list(nid, ilvl, is_ordered, html, list_stack);
        html.push_str("<li>");
        html.push_str(inner);
        html.push_str("</li>\n");
    } else {
        close_all_lists(list_stack, html);
        if inner.trim().is_empty() && tag == "p" {
            return;
        }
        html.push('<');
        html.push_str(tag);
        html.push('>');
        html.push_str(inner);
        html.push_str("</");
        html.push_str(tag);
        html.push_str(">\n");
    }
}

fn style_to_tag(style: &str) -> &'static str {
    match style {
        "Title" | "Heading1" | "Heading 1" => "h1",
        "Heading2" | "Heading 2" => "h2",
        "Heading3" | "Heading 3" => "h3",
        "Heading4" | "Heading 4" => "h4",
        "Heading5" | "Heading 5" => "h5",
        "Heading6" | "Heading 6" => "h6",
        _ => "p",
    }
}

// ── List management ───────────────────────────────────────────────────────────

fn manage_list(
    num_id: &str,
    ilvl: u32,
    is_ordered: bool,
    html: &mut String,
    stack: &mut Vec<(String, u32, bool)>,
) {
    while let Some((top_nid, top_ilvl, top_ordered)) = stack.last() {
        let should_pop = top_nid != num_id || *top_ilvl > ilvl;
        if !should_pop {
            break;
        }
        let tag = if *top_ordered { "ol" } else { "ul" };
        html.push_str(&format!(
            "</{}>
",
            tag
        ));
        stack.pop();
    }

    if stack
        .last()
        .map(|(nid, lvl, _)| nid != num_id || *lvl < ilvl)
        .unwrap_or(true)
    {
        let tag = if is_ordered { "ol" } else { "ul" };
        html.push_str(&format!("<{}>\n", tag));
        stack.push((num_id.to_string(), ilvl, is_ordered));
    }
}

fn close_all_lists(stack: &mut Vec<(String, u32, bool)>, html: &mut String) {
    while let Some((_, _, is_ordered)) = stack.pop() {
        let tag = if is_ordered { "ol" } else { "ul" };
        html.push_str(&format!(
            "</{}>
",
            tag
        ));
    }
}

// ── Table ─────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn emit_table(
    tbl: roxmltree::Node,
    rels: &HashMap<String, (String, String)>,
    images: &HashMap<String, (Vec<u8>, String)>,
    image_dir: Option<&str>,
    image_count: &mut usize,
    html: &mut String,
    scratch: &mut String,
) {
    html.push_str("<table>\n");
    for tr in tbl.children().filter(|n| is_wn(*n, "tr")) {
        html.push_str("<tr>\n");
        for tc in tr.children().filter(|n| is_wn(*n, "tc")) {
            html.push_str("<td>");
            // Join non-blank paragraphs with <br>, writing directly into
            // html via per-paragraph scratch (needed for the blank check).
            let mut first = true;
            for p in tc.children().filter(|n| is_wn(*n, "p")) {
                scratch.clear();
                para_content_html(p, rels, images, image_dir, image_count, scratch);
                if scratch.trim().is_empty() {
                    continue;
                }
                if !first {
                    html.push_str("<br>");
                }
                html.push_str(scratch);
                first = false;
            }
            html.push_str("</td>\n");
        }
        html.push_str("</tr>\n");
    }
    html.push_str("</table>\n");
}

// ── Paragraph content ─────────────────────────────────────────────────────────

/// Append a paragraph's content HTML into `out` (callers reuse one scratch
/// buffer across paragraphs instead of allocating per paragraph).
fn para_content_html(
    para: roxmltree::Node,
    rels: &HashMap<String, (String, String)>,
    images: &HashMap<String, (Vec<u8>, String)>,
    image_dir: Option<&str>,
    image_count: &mut usize,
    out: &mut String,
) {
    for child in para.children().filter(|n| n.is_element()) {
        if is_wn(child, "r") {
            run_html(child, images, image_dir, image_count, out);
        } else if is_wn(child, "hyperlink") {
            let href = child
                .attribute((R, "id"))
                .or_else(|| child.attribute("id"))
                .and_then(|id| rels.get(id))
                .filter(|(kind, _)| kind.ends_with("/hyperlink") || kind == "hyperlink")
                .map(|(_, target)| target.as_str())
                .unwrap_or("");

            if !href.is_empty() {
                out.push_str("<a href=\"");
                out.push_str(&html_escape(href));
                out.push_str("\">");
            }
            for r in child.children().filter(|n| is_wn(*n, "r")) {
                run_html(r, images, image_dir, image_count, out);
            }
            if !href.is_empty() {
                out.push_str("</a>");
            }
        }
    }
}

// ── Run ───────────────────────────────────────────────────────────────────────

/// Append a run's HTML into `out`. Formatting wraps splice their open tag
/// at the run's start position, so no intermediate String is built.
fn run_html(
    run: roxmltree::Node,
    images: &HashMap<String, (Vec<u8>, String)>,
    image_dir: Option<&str>,
    image_count: &mut usize,
    out: &mut String,
) {
    // Inline image via w:drawing
    let drawing = find_wchild(run, "drawing").or_else(|| {
        run.children()
            .find(|n| n.is_element() && n.tag_name().name() == "drawing")
    });

    if let Some(drawing) = drawing {
        if let Some(blip) = find_descendant_ns(drawing, "blip", A) {
            let embed = blip
                .attribute((R, "embed"))
                .or_else(|| blip.attribute("embed"))
                .unwrap_or("");
            if !embed.is_empty() {
                if let Some((img_bytes, ext)) = images.get(embed) {
                    *image_count += 1;
                    let n = *image_count;
                    let ext_out = if ext == "jpeg" { "jpg" } else { ext.as_str() };

                    if let Some(dir) = image_dir {
                        let path = format!("{}/image_{}.{}", dir, n, ext_out);
                        let _ = fs::write(&path, img_bytes);
                        out.push_str("<img src=\"");
                        out.push_str(&path);
                        out.push_str("\" alt=\"image_");
                        out.push_str(&n.to_string());
                        out.push_str("\">");
                    } else {
                        out.push_str(IMG_TOKEN_PREFIX);
                        out.push_str(&n.to_string());
                    }
                }
            }
        }
        return;
    }

    // Text content
    let start = out.len();
    for child in run.children().filter(|n| n.is_element()) {
        if is_wn(child, "t") {
            out.push_str(&html_escape(child.text().unwrap_or("")));
        } else if is_wn(child, "tab") {
            out.push('\t');
        } else if is_wn(child, "br") {
            out.push_str("<br>");
        }
    }

    if out.len() == start {
        return;
    }

    // Character formatting (innermost first, so each open tag splices at
    // the same start position: <strong><em><s>…</s></em></strong>).
    if let Some(rpr) = find_wchild(run, "rPr") {
        let bold = find_wchild(rpr, "b").is_some();
        let italic = find_wchild(rpr, "i").is_some();
        let strike = find_wchild(rpr, "strike").is_some();

        if strike {
            out.insert_str(start, "<s>");
            out.push_str("</s>");
        }
        if italic {
            out.insert_str(start, "<em>");
            out.push_str("</em>");
        }
        if bold {
            out.insert_str(start, "<strong>");
            out.push_str("</strong>");
        }
    }
}

// ── XML helpers ───────────────────────────────────────────────────────────────

fn is_word_namespace(namespace: Option<&str>) -> bool {
    namespace.is_some_and(|value| value == W || value.ends_with("/wordprocessingml/main"))
}

fn is_wn(node: roxmltree::Node, name: &str) -> bool {
    node.is_element()
        && node.tag_name().name() == name
        && is_word_namespace(node.tag_name().namespace())
}

fn wattr<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name() == name && is_word_namespace(attribute.namespace()))
        .map(|attribute| attribute.value())
        .or_else(|| node.attribute(name))
}

fn find_wchild<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|n| {
        n.is_element() && n.tag_name().name() == name && is_word_namespace(n.tag_name().namespace())
    })
}

fn find_descendant_ns<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
    ns: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    for child in node.children().filter(|n| n.is_element()) {
        if child.tag_name().name() == name && child.tag_name().namespace() == Some(ns) {
            return Some(child);
        }
        if let Some(found) = find_descendant_ns(child, name, ns) {
            return Some(found);
        }
    }
    None
}

/// Single-pass HTML escaping; borrows when nothing needs escaping (the
/// overwhelmingly common case for document text runs).
fn html_escape(s: &str) -> std::borrow::Cow<'_, str> {
    let bytes = s.as_bytes();
    let Some(first) = bytes
        .iter()
        .position(|b| matches!(b, b'&' | b'<' | b'>' | b'"'))
    else {
        return std::borrow::Cow::Borrowed(s);
    };
    let mut out = String::with_capacity(s.len() + 8);
    out.push_str(&s[..first]);
    let mut start = first;
    for (i, &b) in bytes.iter().enumerate().skip(first) {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            _ => continue,
        };
        out.push_str(&s[start..i]);
        out.push_str(rep);
        start = i + 1;
    }
    out.push_str(&s[start..]);
    std::borrow::Cow::Owned(out)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Converter, StreamInfo};
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    // 1×1 red PNG bytes
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn base_rels(extra: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  {}
</Relationships>"#,
            extra
        )
    }

    fn wrap_body(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>{}</w:body>
</w:document>"#,
            body
        )
    }

    fn build_docx(doc_xml: &str, rels_xml: &str, image: Option<(&str, &[u8])>) -> Vec<u8> {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();

        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="png" ContentType="image/png"/>
  <Override PartName="/word/document.xml"
    ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#,
        )
        .unwrap();

        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
    Target="word/document.xml"/>
</Relationships>"#,
        )
        .unwrap();

        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(doc_xml.as_bytes()).unwrap();

        zip.start_file("word/_rels/document.xml.rels", opts)
            .unwrap();
        zip.write_all(rels_xml.as_bytes()).unwrap();

        if let Some((path, bytes)) = image {
            zip.start_file(path, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }

        zip.finish().unwrap().into_inner()
    }

    fn simple_docx(text: &str) -> Vec<u8> {
        let doc = wrap_body(&format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", text));
        let rels = base_rels("");
        build_docx(&doc, &rels, None)
    }

    fn docx_with_image() -> Vec<u8> {
        let doc = wrap_body(
            r#"<w:p>
  <w:r><w:t>Hello from DOCX</w:t></w:r>
  <w:r>
    <w:drawing>
      <wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">
        <a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
          <a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">
            <pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
              <pic:blipFill>
                <a:blip r:embed="rId2"/>
              </pic:blipFill>
            </pic:pic>
          </a:graphicData>
        </a:graphic>
      </wp:inline>
    </w:drawing>
  </w:r>
</w:p>"#,
        );
        let rels = base_rels(
            r#"<Relationship Id="rId2"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
    Target="media/image1.png"/>"#,
        );
        build_docx(&doc, &rels, Some(("word/media/image1.png", TINY_PNG)))
    }

    fn info_ext(ext: &str) -> StreamInfo {
        StreamInfo {
            extension: Some(ext.to_string()),
            ..Default::default()
        }
    }

    fn info_with_dir(ext: &str, dir: &str) -> StreamInfo {
        StreamInfo {
            extension: Some(ext.to_string()),
            image_dir: Some(dir.to_string()),
            ..Default::default()
        }
    }

    // ── accepts ───────────────────────────────────────────────────────────────

    #[test]
    fn accepts_docx_extension() {
        assert!(DocxConverter.accepts(&info_ext(".docx")));
        assert!(!DocxConverter.accepts(&info_ext(".pdf")));
    }

    #[test]
    fn accepts_mimetype() {
        let info = StreamInfo {
            mimetype: Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    .to_string(),
            ),
            ..Default::default()
        };
        assert!(DocxConverter.accepts(&info));
    }

    // ── text extraction ───────────────────────────────────────────────────────

    #[test]
    fn extracts_text() {
        let docx = simple_docx("Hello from DOCX");
        let result = DocxConverter.convert(&docx, &info_ext(".docx")).unwrap();
        assert!(
            result.markdown.contains("Hello from DOCX"),
            "got: {:?}",
            result.markdown
        );
    }

    #[test]
    fn text_only_no_image_references() {
        let docx = simple_docx("Hello from DOCX");
        let result = DocxConverter.convert(&docx, &info_ext(".docx")).unwrap();
        assert!(
            !result.markdown.contains("image"),
            "unexpected 'image': {:?}",
            result.markdown
        );
    }

    // ── image placeholder ─────────────────────────────────────────────────────

    #[test]
    fn emits_placeholder_comment_without_image_dir() {
        let docx = docx_with_image();
        let result = DocxConverter.convert(&docx, &info_ext(".docx")).unwrap();
        assert!(
            result.markdown.contains("<!-- image:"),
            "expected placeholder comment, got: {:?}",
            result.markdown
        );
    }

    // ── image file writing ────────────────────────────────────────────────────

    #[test]
    fn saves_image_to_disk_with_image_dir() {
        let dir = std::env::temp_dir().join(format!("docx-rs-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dir_str = dir.to_string_lossy().into_owned();

        let docx = docx_with_image();
        let _result = DocxConverter
            .convert(&docx, &info_with_dir(".docx", &dir_str))
            .unwrap();

        let img_path = dir.join("image_1.png");
        assert!(img_path.exists(), "image file not written");
        assert!(img_path.metadata().unwrap().len() > 0, "image file empty");

        fs::remove_dir_all(&dir).ok();
    }

    // ── table ─────────────────────────────────────────────────────────────────

    #[test]
    fn table_multi_para_cell() {
        let doc = wrap_body(
            r#"<w:tbl>
  <w:tr>
    <w:tc><w:p><w:r><w:t>Header</w:t></w:r></w:p></w:tc>
  </w:tr>
  <w:tr>
    <w:tc>
      <w:p><w:r><w:t>Line 1</w:t></w:r></w:p>
      <w:p><w:r><w:t>Line 2</w:t></w:r></w:p>
    </w:tc>
  </w:tr>
</w:tbl>"#,
        );
        let rels = base_rels("");
        let docx = build_docx(&doc, &rels, None);
        let result = DocxConverter.convert(&docx, &info_ext(".docx")).unwrap();

        let md = &result.markdown;
        assert!(md.contains("Line 1"), "missing Line 1: {:?}", md);
        assert!(md.contains("Line 2"), "missing Line 2: {:?}", md);
        // Both lines should NOT appear as separate table rows
        assert!(
            !md.contains("| Line 1") || !md.contains("| Line 2"),
            "Lines should be in same cell, got: {:?}",
            md
        );
    }

    // ── formatting ────────────────────────────────────────────────────────────

    #[test]
    fn bold_italic_text_present() {
        let doc = wrap_body(
            r#"<w:p>
  <w:r>
    <w:rPr><w:b/><w:i/></w:rPr>
    <w:t>bold italic</w:t>
  </w:r>
</w:p>"#,
        );
        let rels = base_rels("");
        let docx = build_docx(&doc, &rels, None);
        let result = DocxConverter.convert(&docx, &info_ext(".docx")).unwrap();
        assert!(
            result.markdown.contains("bold italic"),
            "{:?}",
            result.markdown
        );
    }

    // ── headings ─────────────────────────────────────────────────────────────

    #[test]
    fn heading_text_present() {
        let doc = wrap_body(
            r#"<w:p>
  <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
  <w:r><w:t>Chapter One</w:t></w:r>
</w:p>"#,
        );
        let rels = base_rels("");
        let docx = build_docx(&doc, &rels, None);
        let result = DocxConverter.convert(&docx, &info_ext(".docx")).unwrap();
        assert!(
            result.markdown.contains("Chapter One"),
            "{:?}",
            result.markdown
        );
    }

    // ── error path ────────────────────────────────────────────────────────────

    #[test]
    fn invalid_zip_is_error() {
        let err = DocxConverter
            .convert(b"not a zip", &info_ext(".docx"))
            .unwrap_err();
        assert!(err.to_string().contains("Invalid DOCX"), "{}", err);
    }

    fn custom_path_docx(part: &str, namespace: &str, text: &str) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="{part}"/></Relationships>"#).as_bytes()).unwrap();
        zip.start_file(part, opts).unwrap();
        zip.write_all(format!(r#"<w:document xmlns:w="{namespace}"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#).as_bytes()).unwrap();
        let (dir, name) = part.rsplit_once('/').unwrap_or(("", part));
        let rels_path = if dir.is_empty() {
            format!("_rels/{name}.rels")
        } else {
            format!("{dir}/_rels/{name}.rels")
        };
        zip.start_file(rels_path, opts).unwrap();
        zip.write_all(base_rels("").as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn resolves_main_document_from_root_relationship() {
        let bytes = custom_path_docx(
            "content/main.xml",
            "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
            "Relocated heading",
        );
        let markdown = DocxConverter
            .convert(&bytes, &info_ext(".docx"))
            .unwrap()
            .markdown;
        assert!(markdown.contains("Relocated heading"), "{markdown}");
    }

    #[test]
    fn accepts_strict_wordprocessing_namespace() {
        let bytes = custom_path_docx(
            "word/document.xml",
            "http://purl.oclc.org/ooxml/wordprocessingml/main",
            "Strict heading",
        );
        let markdown = DocxConverter
            .convert(&bytes, &info_ext(".docx"))
            .unwrap()
            .markdown;
        assert!(markdown.contains("Strict heading"), "{markdown}");
    }

    #[test]
    fn strict_relationship_attributes_preserve_hyperlinks() {
        let xml = r#"<w:document xmlns:w="http://purl.oclc.org/ooxml/wordprocessingml/main" xmlns:r="http://purl.oclc.org/ooxml/officeDocument/relationships"><w:body><w:p><w:hyperlink r:id="rId1"><w:r><w:t>strict link</w:t></w:r></w:hyperlink></w:p></w:body></w:document>"#;
        let rels = HashMap::from([(
            "rId1".to_string(),
            ("hyperlink".to_string(), "https://example.com".to_string()),
        )]);
        let html = doc_to_html(xml, &rels, &HashMap::new(), &HashMap::new(), None, &mut 0).unwrap();
        assert!(html.contains("href=\"https://example.com\""), "{html}");
    }
}
