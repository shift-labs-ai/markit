use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

const MIMETYPES: &[&str] =
    &["application/vnd.openxmlformats-officedocument.presentationml.presentation"];

/// Namespace URI for the `r:` prefix used in OOXML relationship attributes.
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

pub struct PptxConverter;

impl Converter for PptxConverter {
    fn name(&self) -> &'static str {
        "pptx"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        matches!(info.extension.as_deref(), Some(".pptx"))
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
        // Read all zip entries into memory so we can access them without
        // fighting the borrow checker across multiple reads.
        let files = read_zip_files(input)?;

        // ── 1. Parse presentation.xml ────────────────────────────────────────
        let pres_bytes = files
            .get("ppt/presentation.xml")
            .ok_or_else(|| anyhow!("Invalid PPTX: missing presentation.xml"))?;
        let pres_xml = String::from_utf8_lossy(pres_bytes);
        let pres_doc = roxmltree::Document::parse(&pres_xml)
            .map_err(|e| anyhow!("Failed to parse presentation.xml: {}", e))?;

        // Collect slide rIds in slide-show order.
        let mut sld_r_ids: Vec<String> = Vec::new();
        let pres_el = pres_doc.root_element();
        if let Some(sld_id_lst) = find_path(pres_el, &["sldIdLst"]) {
            for node in sld_id_lst
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == "sldId")
            {
                if let Some(r_id) = node.attribute((R_NS, "id")) {
                    sld_r_ids.push(r_id.to_string());
                }
            }
        }

        // ── 2. Parse presentation rels → rId → target path ──────────────────
        let rel_map = match files.get("ppt/_rels/presentation.xml.rels") {
            Some(bytes) => parse_rels(&String::from_utf8_lossy(bytes)),
            None => HashMap::new(),
        };

        // Build ordered slide paths: "ppt/slides/slideN.xml"
        let mut slide_paths: Vec<String> = Vec::new();
        for r_id in &sld_r_ids {
            if let Some(target) = rel_map.get(r_id) {
                // target is relative to ppt/, e.g. "slides/slide1.xml"
                slide_paths.push(format!("ppt/{}", target));
            }
        }

        // Fallback: discover slide files by name pattern, sorted numerically.
        if slide_paths.is_empty() {
            let mut slide_files: Vec<String> =
                files.keys().filter(|f| is_slide_path(f)).cloned().collect();
            slide_files.sort_by_key(|f| extract_slide_number(f));
            slide_paths = slide_files;
        }

        // ── 3. Ensure image output dir exists ───────────────────────────────
        let image_dir = info.image_dir.as_deref();
        if let Some(dir) = image_dir {
            std::fs::create_dir_all(dir)?;
        }

        // ── 4. Process each slide ────────────────────────────────────────────
        let mut sections: Vec<String> = Vec::new();
        let mut image_count: usize = 0;

        for (i, slide_path) in slide_paths.iter().enumerate() {
            let slide_num = i + 1;

            let slide_bytes = match files.get(slide_path.as_str()) {
                Some(b) => b,
                None => continue,
            };
            let slide_xml = String::from_utf8_lossy(slide_bytes);
            let slide_doc = match roxmltree::Document::parse(&slide_xml) {
                Ok(d) => d,
                Err(_) => continue,
            };

            // Parse slide-level rels for image r:embed lookups.
            let slide_rels_path =
                slide_path.replace("slides/slide", "slides/_rels/slide") + ".rels";
            let slide_rel_map = match files.get(&slide_rels_path) {
                Some(bytes) => parse_rels(&String::from_utf8_lossy(bytes)),
                None => HashMap::new(),
            };

            let mut slide_lines: Vec<String> = vec![format!("<!-- Slide {} -->", slide_num)];

            let sld_el = slide_doc.root_element(); // p:sld
            let sp_tree_opt = find_path(sld_el, &["cSld", "spTree"]);

            if let Some(sp_tree) = sp_tree_opt {
                // ── Text shapes (p:sp) ──
                let mut is_title = true;
                for shape in sp_tree
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "sp")
                {
                    let text = extract_text(shape);
                    if text.is_empty() {
                        continue;
                    }
                    if is_title {
                        slide_lines.push(format!("# {}", text));
                        is_title = false;
                    } else {
                        slide_lines.push(text);
                    }
                }

                // ── Embedded images (p:pic) ──
                for pic in sp_tree
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "pic")
                {
                    // r:embed on a:blip
                    let r_embed = find_path(pic, &["blipFill", "blip"])
                        .and_then(|n| n.attribute((R_NS, "embed")))
                        .map(|s| s.to_string());
                    let r_embed = match r_embed {
                        Some(e) => e,
                        None => continue,
                    };

                    let target = match slide_rel_map.get(&r_embed) {
                        Some(t) => t.clone(),
                        None => continue,
                    };

                    // Resolve relative path against ppt/slides/
                    let raw_path = if let Some(stripped) = target.strip_prefix('/') {
                        stripped.to_string()
                    } else {
                        format!("ppt/slides/{}", target)
                    };
                    let normalized = normalize_path(&raw_path);

                    // Skip if the image is not in the archive.
                    if !files.contains_key(&normalized) {
                        continue;
                    }

                    image_count += 1;

                    // Name: try nvSpPr/cNvPr then nvPicPr/cNvPr, then fallback.
                    let cnv_pr = find_path(pic, &["nvSpPr", "cNvPr"])
                        .or_else(|| find_path(pic, &["nvPicPr", "cNvPr"]));
                    let name = cnv_pr
                        .and_then(|n| n.attribute("name"))
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("image_{}", image_count));

                    if let Some(dir) = image_dir {
                        let ext = Path::new(&normalized)
                            .extension()
                            .map(|e| e.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "png".to_string());
                        let filename = format!("slide{}_{}.{}", slide_num, image_count, ext);
                        let filepath = Path::new(dir).join(&filename);
                        match std::fs::write(&filepath, &files[&normalized]) {
                            Ok(()) => {
                                slide_lines.push(format!("![{}]({})", name, filepath.display()))
                            }
                            Err(_) => slide_lines
                                .push(format!("<!-- image: {} (slide {}) -->", name, slide_num)),
                        }
                    } else {
                        slide_lines.push(format!("<!-- image: {} (slide {}) -->", name, slide_num));
                    }
                }

                // ── Tables (p:graphicFrame → a:tbl) ──
                for gf in sp_tree
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "graphicFrame")
                {
                    if let Some(table_md) = extract_table(gf) {
                        slide_lines.push(table_md);
                    }
                }
            }

            // ── Slide notes ──
            let note_path = slide_path.replace("slides/slide", "notesSlides/notesSlide");
            if let Some(note_bytes) = files.get(&note_path) {
                let note_xml = String::from_utf8_lossy(note_bytes);
                if let Ok(note_doc) = roxmltree::Document::parse(&note_xml) {
                    let note_el = note_doc.root_element(); // p:notes
                    if let Some(sp_tree) = find_path(note_el, &["cSld", "spTree"]) {
                        let mut note_texts: Vec<String> = Vec::new();
                        for shape in sp_tree
                            .children()
                            .filter(|n| n.is_element() && n.tag_name().name() == "sp")
                        {
                            // Skip the slide-image placeholder shape.
                            let ph_type = find_path(shape, &["nvSpPr", "nvPr", "ph"])
                                .and_then(|n| n.attribute("type"));
                            if ph_type == Some("sldImg") {
                                continue;
                            }
                            let t = extract_text(shape);
                            if !t.is_empty() {
                                note_texts.push(t);
                            }
                        }
                        if !note_texts.is_empty() {
                            slide_lines.push("\n### Notes:".to_string());
                            slide_lines.push(note_texts.join("\n"));
                        }
                    }
                }
            }

            sections.push(slide_lines.join("\n"));
        }

        Ok(ConversionResult::markdown(
            sections.join("\n\n").trim().to_string(),
        ))
    }
}

// ── Zip helpers ──────────────────────────────────────────────────────────────

fn read_zip_files(input: &[u8]) -> Result<HashMap<String, Vec<u8>>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(input))?;
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_string();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        files.insert(name, buf);
    }
    Ok(files)
}

// ── XML helpers ──────────────────────────────────────────────────────────────

/// Parse an OOXML `.rels` file and return Id → Target map.
fn parse_rels(xml: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(doc) = roxmltree::Document::parse(xml) {
        let root = doc.root_element();
        for rel in root
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "Relationship")
        {
            if let (Some(id), Some(target)) = (rel.attribute("Id"), rel.attribute("Target")) {
                map.insert(id.to_string(), target.to_string());
            }
        }
    }
    map
}

/// Walk a chain of child element local-names from `node`.
fn find_path<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    names: &[&str],
) -> Option<roxmltree::Node<'a, 'input>> {
    let mut current = node;
    for &name in names {
        current = current
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == name)?;
    }
    Some(current)
}

/// Extract all text from a `p:sp` shape's `p:txBody`, joining paragraphs with
/// newlines and runs within a paragraph with no separator.
fn extract_text(shape: roxmltree::Node) -> String {
    let tx_body = match find_path(shape, &["txBody"]) {
        Some(tb) => tb,
        None => return String::new(),
    };

    let mut lines: Vec<String> = Vec::new();
    for p in tx_body
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "p")
    {
        let mut parts: Vec<String> = Vec::new();
        for r in p
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "r")
        {
            if let Some(t) = r
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "t")
            {
                parts.push(t.text().unwrap_or("").to_string());
            }
        }
        if !parts.is_empty() {
            lines.push(parts.join(""));
        }
    }

    lines.join("\n").trim().to_string()
}

/// Convert an `a:tbl` found inside a `p:graphicFrame` to a markdown table.
fn extract_table(gf: roxmltree::Node) -> Option<String> {
    let tbl = find_path(gf, &["graphic", "graphicData", "tbl"])?;

    let mut md_rows: Vec<Vec<String>> = Vec::new();
    for tr in tbl
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "tr")
    {
        let mut cell_texts: Vec<String> = Vec::new();
        for tc in tr
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "tc")
        {
            if let Some(tx_body) = find_path(tc, &["txBody"]) {
                let mut parts: Vec<String> = Vec::new();
                for p in tx_body
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "p")
                {
                    for r in p
                        .children()
                        .filter(|n| n.is_element() && n.tag_name().name() == "r")
                    {
                        if let Some(t) = r
                            .children()
                            .find(|n| n.is_element() && n.tag_name().name() == "t")
                        {
                            parts.push(t.text().unwrap_or("").to_string());
                        }
                    }
                }
                cell_texts.push(parts.join(" "));
            } else {
                cell_texts.push(String::new());
            }
        }
        md_rows.push(cell_texts);
    }

    if md_rows.is_empty() {
        return None;
    }

    let (header, body) = md_rows.split_first()?;
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("| {} |", header.join(" | ")));
    lines.push(format!(
        "| {} |",
        header.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
    ));
    for row in body {
        let mut row = row.clone();
        while row.len() < header.len() {
            row.push(String::new());
        }
        lines.push(format!("| {} |", row.join(" | ")));
    }

    Some(lines.join("\n"))
}

// ── Path helpers ─────────────────────────────────────────────────────────────

/// Collapse `\..` segments: `ppt/slides/../media/image1.png` →
/// `ppt/media/image1.png`.
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        if seg == ".." {
            parts.pop();
        } else if !seg.is_empty() {
            parts.push(seg);
        }
    }
    parts.join("/")
}

fn is_slide_path(path: &str) -> bool {
    if let Some(rest) = path.strip_prefix("ppt/slides/slide") {
        if let Some(digits) = rest.strip_suffix(".xml") {
            return !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

fn extract_slide_number(path: &str) -> u32 {
    path.strip_prefix("ppt/slides/slide")
        .and_then(|s| s.strip_suffix(".xml"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    /// 1×1 red PNG bytes.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn stored() -> SimpleFileOptions {
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
    }

    /// Helper: create a unique temp directory, return its path as String.
    fn tempdir() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("pptx-test-{}", ts));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn info_ext(ext: &str) -> StreamInfo {
        StreamInfo {
            extension: Some(ext.to_string()),
            ..Default::default()
        }
    }

    /// Build a minimal PPTX (one slide, one text shape, optional image).
    fn build_pptx(with_image: bool, image_name: &str) -> Vec<u8> {
        let cursor = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = stored();

        zip.start_file("ppt/presentation.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation
    xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst>
</p:presentation>"#,
        )
        .unwrap();

        zip.start_file("ppt/_rels/presentation.xml.rels", opts)
            .unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide"
    Target="slides/slide1.xml"/>
</Relationships>"#,
        )
        .unwrap();

        // Slide XML — conditionally includes a p:pic element.
        let pic_xml = if with_image {
            format!(
                r#"<p:pic>
  <p:nvPicPr>
    <p:cNvPr id="4" name="{name}"/>
    <p:cNvPicPr/><p:nvPr/>
  </p:nvPicPr>
  <p:blipFill>
    <a:blip r:embed="rId2"/>
  </p:blipFill>
  <p:spPr/>
</p:pic>"#,
                name = image_name
            )
        } else {
            String::new()
        };

        zip.start_file("ppt/slides/slide1.xml", opts).unwrap();
        zip.write_all(
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld
    xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
    xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p><a:r><a:t>Hello World</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
      {pic}
    </p:spTree>
  </p:cSld>
</p:sld>"#,
                pic = pic_xml
            )
            .as_bytes(),
        )
        .unwrap();

        if with_image {
            zip.start_file("ppt/slides/_rels/slide1.xml.rels", opts)
                .unwrap();
            zip.write_all(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
    Target="../media/image1.png"/>
</Relationships>"#,
            )
            .unwrap();

            zip.start_file("ppt/media/image1.png", opts).unwrap();
            zip.write_all(TINY_PNG).unwrap();
        }

        zip.finish().unwrap().into_inner()
    }

    // ── Core TS-mirrored test cases ──────────────────────────────────────────

    #[test]
    fn extracts_text_from_shapes() {
        let bytes = build_pptx(false, "");
        let result = PptxConverter
            .convert(&bytes, &info_ext(".pptx"), &MarkitOptions::default())
            .unwrap();
        assert!(
            result.markdown.contains("Hello World"),
            "markdown should contain slide text:\n{}",
            result.markdown
        );
    }

    #[test]
    fn emits_image_placeholder_without_image_dir() {
        let bytes = build_pptx(true, "Logo");
        let result = PptxConverter
            .convert(&bytes, &info_ext(".pptx"), &MarkitOptions::default())
            .unwrap();
        assert!(
            result.markdown.contains("<!-- image: Logo (slide 1) -->"),
            "expected placeholder comment; got:\n{}",
            result.markdown
        );
    }

    #[test]
    fn saves_image_to_disk_with_image_dir() {
        let dir = tempdir();
        let bytes = build_pptx(true, "Picture 1");
        let si = StreamInfo {
            extension: Some(".pptx".to_string()),
            image_dir: Some(dir.clone()),
            ..Default::default()
        };

        let result = PptxConverter
            .convert(&bytes, &si, &MarkitOptions::default())
            .unwrap();

        assert!(
            result.markdown.contains("![Picture 1]"),
            "should have inline image:\n{}",
            result.markdown
        );
        assert!(
            result.markdown.contains(&dir),
            "should reference output dir:\n{}",
            result.markdown
        );

        let img_path = std::path::Path::new(&dir).join("slide1_1.png");
        assert!(img_path.exists(), "slide1_1.png should have been written");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn text_only_slides_have_no_image_references() {
        let bytes = build_pptx(false, "");
        let result = PptxConverter
            .convert(&bytes, &info_ext(".pptx"), &MarkitOptions::default())
            .unwrap();
        assert!(
            !result.markdown.contains("image"),
            "text-only slide should not reference images:\n{}",
            result.markdown
        );
    }

    // ── Additional coverage ──────────────────────────────────────────────────

    #[test]
    fn slide_comment_present() {
        let bytes = build_pptx(false, "");
        let result = PptxConverter
            .convert(&bytes, &info_ext(".pptx"), &MarkitOptions::default())
            .unwrap();
        assert!(result.markdown.starts_with("<!-- Slide 1 -->"));
    }

    #[test]
    fn first_text_shape_becomes_h1() {
        let bytes = build_pptx(false, "");
        let result = PptxConverter
            .convert(&bytes, &info_ext(".pptx"), &MarkitOptions::default())
            .unwrap();
        assert!(
            result.markdown.contains("# Hello World"),
            "first shape should be H1:\n{}",
            result.markdown
        );
    }

    #[test]
    fn accepts_pptx_extension() {
        assert!(PptxConverter.accepts(&info_ext(".pptx")));
        assert!(!PptxConverter.accepts(&info_ext(".docx")));
        assert!(!PptxConverter.accepts(&info_ext(".pdf")));
    }

    #[test]
    fn accepts_pptx_mimetype() {
        let si = StreamInfo {
            mimetype: Some(
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                    .to_string(),
            ),
            ..Default::default()
        };
        assert!(PptxConverter.accepts(&si));
    }

    #[test]
    fn missing_presentation_xml_is_error() {
        let cursor = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file("dummy.txt", stored()).unwrap();
        std::io::Write::write_all(&mut zip, b"hello").unwrap();
        let bytes = zip.finish().unwrap().into_inner();

        let err = PptxConverter
            .convert(&bytes, &info_ext(".pptx"), &MarkitOptions::default())
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Invalid PPTX: missing presentation.xml"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn table_renders_as_markdown() {
        let cursor = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = stored();

        zip.start_file("ppt/presentation.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<p:presentation
    xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst>
</p:presentation>"#,
        )
        .unwrap();

        zip.start_file("ppt/_rels/presentation.xml.rels", opts)
            .unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide"
    Target="slides/slide1.xml"/>
</Relationships>"#,
        )
        .unwrap();

        zip.start_file("ppt/slides/slide1.xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld
    xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
    xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:graphicFrame>
        <a:graphic>
          <a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table">
            <a:tbl>
              <a:tr>
                <a:tc><a:txBody><a:p><a:r><a:t>Name</a:t></a:r></a:p></a:txBody></a:tc>
                <a:tc><a:txBody><a:p><a:r><a:t>Age</a:t></a:r></a:p></a:txBody></a:tc>
              </a:tr>
              <a:tr>
                <a:tc><a:txBody><a:p><a:r><a:t>Alice</a:t></a:r></a:p></a:txBody></a:tc>
                <a:tc><a:txBody><a:p><a:r><a:t>30</a:t></a:r></a:p></a:txBody></a:tc>
              </a:tr>
            </a:tbl>
          </a:graphicData>
        </a:graphic>
      </p:graphicFrame>
    </p:spTree>
  </p:cSld>
</p:sld>"#,
        )
        .unwrap();

        let bytes = zip.finish().unwrap().into_inner();
        let result = PptxConverter
            .convert(&bytes, &info_ext(".pptx"), &MarkitOptions::default())
            .unwrap();

        assert!(
            result.markdown.contains("| Name | Age |"),
            "header row missing"
        );
        assert!(
            result.markdown.contains("| --- | --- |"),
            "separator missing"
        );
        assert!(
            result.markdown.contains("| Alice | 30 |"),
            "data row missing"
        );
    }

    #[test]
    fn path_normalization() {
        assert_eq!(
            normalize_path("ppt/slides/../media/image1.png"),
            "ppt/media/image1.png"
        );
        assert_eq!(
            normalize_path("ppt/media/image1.png"),
            "ppt/media/image1.png"
        );
        assert_eq!(normalize_path("a/b/c/../../d"), "a/d");
    }

    #[test]
    fn fallback_slide_order() {
        assert_eq!(extract_slide_number("ppt/slides/slide9.xml"), 9);
        assert_eq!(extract_slide_number("ppt/slides/slide10.xml"), 10);
        assert!(
            extract_slide_number("ppt/slides/slide10.xml")
                > extract_slide_number("ppt/slides/slide9.xml")
        );
        assert!(!is_slide_path("ppt/slides/slideshow.xml"));
        assert!(is_slide_path("ppt/slides/slide1.xml"));
        assert!(is_slide_path("ppt/slides/slide12.xml"));
    }
}
