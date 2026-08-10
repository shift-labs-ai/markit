use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::io::Read;

use crate::types::{ConversionResult, Converter, StreamInfo};
use crate::utils::html_to_md::{html_to_markdown, normalize_tables_html};

const EXTENSIONS: &[&str] = &[".epub"];
const MIMETYPES: &[&str] = &[
    "application/epub",
    "application/epub+zip",
    "application/x-epub+zip",
];

pub struct EpubConverter;

// ── Converter impl ────────────────────────────────────────────────────────────

impl Converter for EpubConverter {
    fn name(&self) -> &'static str {
        "epub"
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
        let cursor = std::io::Cursor::new(input);
        let mut archive = zip::ZipArchive::new(cursor)?;

        // 1. META-INF/container.xml → OPF path
        let container_xml = read_zip_entry(&mut archive, "META-INF/container.xml")
            .ok_or_else(|| anyhow!("Invalid EPUB: missing container.xml"))?;

        let opf_path = parse_container_xml(&container_xml)?;

        // 2. Parse OPF
        let opf_xml = read_zip_entry(&mut archive, &opf_path)
            .ok_or_else(|| anyhow!("Invalid EPUB: missing content.opf"))?;

        let (metadata, manifest, spine_order) = parse_opf(&opf_xml)?;

        // 3. Base directory for resolving relative hrefs
        let base_path = match opf_path.rfind('/') {
            Some(pos) => opf_path[..pos].to_string(),
            None => String::new(),
        };

        let mut sections: Vec<String> = Vec::new();

        // 4. Metadata block
        let meta_lines: Vec<String> = [
            ("Title", metadata.title.as_deref()),
            ("Authors", metadata.authors.as_deref()),
            ("Language", metadata.language.as_deref()),
            ("Publisher", metadata.publisher.as_deref()),
            ("Date", metadata.date.as_deref()),
            ("Description", metadata.description.as_deref()),
        ]
        .iter()
        .filter_map(|(key, value)| value.map(|v| format!("**{}:** {}", key, v)))
        .collect();

        if !meta_lines.is_empty() {
            sections.push(meta_lines.join("\n"));
        }

        // 5. Convert spine chapters
        for idref in &spine_order {
            let href = match manifest.get(idref.as_str()) {
                Some(h) => h,
                None => continue,
            };

            let file_path = if base_path.is_empty() {
                href.clone()
            } else {
                format!("{}/{}", base_path, href)
            };

            let html = match read_zip_entry(&mut archive, &file_path) {
                Some(h) => h,
                None => continue,
            };

            // Strip <script> and <style> before conversion (matches TS regex)
            let cleaned = strip_script_style(&html);
            let normalized = normalize_tables_html(&cleaned);
            let md = html_to_markdown(&normalized).trim().to_string();
            if !md.is_empty() {
                sections.push(md);
            }
        }

        Ok(ConversionResult {
            markdown: sections.join("\n\n").trim().to_string(),
            title: metadata.title,
        })
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

struct EpubMetadata {
    title: Option<String>,
    authors: Option<String>,
    language: Option<String>,
    publisher: Option<String>,
    date: Option<String>,
    description: Option<String>,
}

/// Read a named entry from a ZIP archive as UTF-8 text.
fn read_zip_entry<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    path: &str,
) -> Option<String> {
    let mut file = archive.by_name(path).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    Some(contents)
}

/// Parse META-INF/container.xml and return the OPF full-path.
fn parse_container_xml(xml: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| anyhow!("Invalid EPUB container.xml: {}", e))?;

    for node in doc.descendants() {
        if node.tag_name().name() == "rootfile" {
            if let Some(path) = node.attribute("full-path") {
                return Ok(path.to_string());
            }
        }
    }

    Err(anyhow!("Invalid EPUB: missing rootfile path"))
}

/// Return the text content of the first DC element with the given local name.
fn dc_text(doc: &roxmltree::Document, local: &str) -> Option<String> {
    for node in doc.descendants() {
        let tag = node.tag_name();
        if tag.name() == local && is_dc_namespace(tag.namespace()) {
            let text: String = node
                .children()
                .filter(|n| n.is_text())
                .map(|n| n.text().unwrap_or(""))
                .collect();
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// Return text content of ALL DC elements with the given local name.
fn dc_text_all(doc: &roxmltree::Document, local: &str) -> Vec<String> {
    let mut results = Vec::new();
    for node in doc.descendants() {
        let tag = node.tag_name();
        if tag.name() == local && is_dc_namespace(tag.namespace()) {
            let text: String = node
                .children()
                .filter(|n| n.is_text())
                .map(|n| n.text().unwrap_or(""))
                .collect();
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                results.push(trimmed);
            }
        }
    }
    results
}

/// True for any Dublin Core namespace URI.
fn is_dc_namespace(ns: Option<&str>) -> bool {
    ns.is_some_and(|n| n.contains("purl.org/dc") || n.contains("dublin"))
}

/// Parse an OPF file and return (metadata, manifest id→href, spine idref order).
fn parse_opf(xml: &str) -> Result<(EpubMetadata, HashMap<String, String>, Vec<String>)> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| anyhow!("Invalid EPUB OPF: {}", e))?;

    // Metadata
    let creators = dc_text_all(&doc, "creator");
    let metadata = EpubMetadata {
        title: dc_text(&doc, "title"),
        authors: if creators.is_empty() {
            None
        } else {
            Some(creators.join(", "))
        },
        language: dc_text(&doc, "language"),
        publisher: dc_text(&doc, "publisher"),
        date: dc_text(&doc, "date"),
        description: dc_text(&doc, "description"),
    };

    // Manifest: <item id="..." href="..."/>
    let mut manifest: HashMap<String, String> = HashMap::new();
    for node in doc.descendants() {
        if node.tag_name().name() == "item" {
            if let (Some(id), Some(href)) = (node.attribute("id"), node.attribute("href")) {
                manifest.insert(id.to_string(), href.to_string());
            }
        }
    }

    // Spine: <itemref idref="..."/>
    let spine_order: Vec<String> = doc
        .descendants()
        .filter(|n| n.tag_name().name() == "itemref")
        .filter_map(|n| n.attribute("idref").map(str::to_string))
        .collect();

    Ok((metadata, manifest, spine_order))
}

/// Strip <script …>…</script> and <style …>…</style> blocks (case-insensitive, dotall).
fn strip_script_style(html: &str) -> String {
    let re_script = regex::Regex::new(r"(?is)<script[\s\S]*?</script>").unwrap();
    let re_style = regex::Regex::new(r"(?is)<style[\s\S]*?</style>").unwrap();
    let s = re_script.replace_all(html, "");
    re_style.replace_all(&s, "").into_owned()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::FileOptions;

    /// Build a minimal in-memory EPUB archive.
    fn build_epub(
        container_xml: &str,
        opf_path: &str,
        opf_xml: &str,
        chapters: &[(&str, &str)],
    ) -> Vec<u8> {
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);

        zip.start_file("META-INF/container.xml", opts).unwrap();
        zip.write_all(container_xml.as_bytes()).unwrap();

        zip.start_file(opf_path, opts).unwrap();
        zip.write_all(opf_xml.as_bytes()).unwrap();

        for (path, content) in chapters {
            zip.start_file(*path, opts).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }

        zip.finish().unwrap().into_inner()
    }

    fn simple_container(opf_path: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="{}" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#,
            opf_path
        )
    }

    fn simple_opf(title: &str, authors: &[&str], chapters: &[(&str, &str)]) -> String {
        let creator_tags: String = authors
            .iter()
            .map(|a| format!("    <dc:creator>{}</dc:creator>\n", a))
            .collect();
        let item_tags: String = chapters
            .iter()
            .enumerate()
            .map(|(i, (href, _))| {
                format!(
                    "    <item id=\"ch{}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>\n",
                    i, href
                )
            })
            .collect();
        let itemref_tags: String = chapters
            .iter()
            .enumerate()
            .map(|(i, _)| format!("    <itemref idref=\"ch{}\"/>\n", i))
            .collect();

        format!(
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata>
    <dc:title>{}</dc:title>
{}    <dc:language>en</dc:language>
  </metadata>
  <manifest>
{}  </manifest>
  <spine>
{}  </spine>
</package>"#,
            title, creator_tags, item_tags, itemref_tags
        )
    }

    fn info_epub() -> StreamInfo {
        StreamInfo {
            extension: Some(".epub".to_string()),
            ..Default::default()
        }
    }

    // ── accepts() ────────────────────────────────────────────────────────────

    #[test]
    fn accepts_epub_extension() {
        assert!(EpubConverter.accepts(&info_epub()));
    }

    #[test]
    fn accepts_epub_mimetype() {
        let info = StreamInfo {
            mimetype: Some("application/epub+zip".to_string()),
            ..Default::default()
        };
        assert!(EpubConverter.accepts(&info));
    }

    #[test]
    fn rejects_non_epub() {
        let info = StreamInfo {
            extension: Some(".pdf".to_string()),
            ..Default::default()
        };
        assert!(!EpubConverter.accepts(&info));
    }

    // ── Error cases ───────────────────────────────────────────────────────────

    #[test]
    fn error_missing_container_xml() {
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("dummy.txt", opts).unwrap();
        zip.write_all(b"nothing").unwrap();
        let epub_bytes = zip.finish().unwrap().into_inner();

        let err = EpubConverter
            .convert(&epub_bytes, &info_epub())
            .unwrap_err();
        assert!(
            err.to_string().contains("missing container.xml"),
            "expected 'missing container.xml', got: {}",
            err
        );
    }

    #[test]
    fn error_missing_rootfile_path() {
        let container = r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles/>
</container>"#;

        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("META-INF/container.xml", opts).unwrap();
        zip.write_all(container.as_bytes()).unwrap();
        let epub_bytes = zip.finish().unwrap().into_inner();

        let err = EpubConverter
            .convert(&epub_bytes, &info_epub())
            .unwrap_err();
        assert!(
            err.to_string().contains("missing rootfile path"),
            "got: {}",
            err
        );
    }

    #[test]
    fn error_missing_content_opf() {
        let container = simple_container("OEBPS/content.opf");
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("META-INF/container.xml", opts).unwrap();
        zip.write_all(container.as_bytes()).unwrap();
        let epub_bytes = zip.finish().unwrap().into_inner();

        let err = EpubConverter
            .convert(&epub_bytes, &info_epub())
            .unwrap_err();
        assert!(
            err.to_string().contains("missing content.opf"),
            "got: {}",
            err
        );
    }

    // ── Metadata ─────────────────────────────────────────────────────────────

    #[test]
    fn metadata_title_in_result() {
        let chapters = [("ch1.xhtml", "<html><body><p>Hello</p></body></html>")];
        let container = simple_container("content.opf");
        let opf = simple_opf("My Great Book", &["Alice"], &chapters);
        let epub = build_epub(&container, "content.opf", &opf, &chapters);

        let result = EpubConverter.convert(&epub, &info_epub()).unwrap();

        assert_eq!(result.title.as_deref(), Some("My Great Book"));
    }

    #[test]
    fn metadata_block_contains_title_and_language() {
        let chapters = [("ch1.xhtml", "<html><body><p>Text</p></body></html>")];
        let container = simple_container("content.opf");
        let opf = simple_opf("Test Book", &[], &chapters);
        let epub = build_epub(&container, "content.opf", &opf, &chapters);

        let result = EpubConverter.convert(&epub, &info_epub()).unwrap();

        assert!(
            result.markdown.contains("**Title:** Test Book"),
            "missing title in metadata block: {}",
            result.markdown
        );
        assert!(
            result.markdown.contains("**Language:** en"),
            "missing language in metadata block: {}",
            result.markdown
        );
    }

    #[test]
    fn metadata_multiple_authors_joined() {
        let chapters = [("ch1.xhtml", "<html><body><p>Hello</p></body></html>")];
        let container = simple_container("content.opf");
        let opf = simple_opf("Multi-Author", &["Alice", "Bob"], &chapters);
        let epub = build_epub(&container, "content.opf", &opf, &chapters);

        let result = EpubConverter.convert(&epub, &info_epub()).unwrap();

        assert!(
            result.markdown.contains("**Authors:** Alice, Bob"),
            "authors not joined: {}",
            result.markdown
        );
    }

    // ── Spine order & chapter content ─────────────────────────────────────────

    #[test]
    fn chapters_appear_in_spine_order() {
        let chapters = [
            (
                "ch1.xhtml",
                "<html><body><p>Chapter One Content</p></body></html>",
            ),
            (
                "ch2.xhtml",
                "<html><body><p>Chapter Two Content</p></body></html>",
            ),
        ];
        let container = simple_container("content.opf");
        let opf = simple_opf("Ordered Book", &[], &chapters);
        let epub = build_epub(&container, "content.opf", &opf, &chapters);

        let result = EpubConverter.convert(&epub, &info_epub()).unwrap();

        let pos_ch1 = result
            .markdown
            .find("Chapter One Content")
            .expect("ch1 text missing");
        let pos_ch2 = result
            .markdown
            .find("Chapter Two Content")
            .expect("ch2 text missing");
        assert!(pos_ch1 < pos_ch2, "ch1 should appear before ch2");
    }

    #[test]
    fn chapter_text_present_in_output() {
        let chapters = [(
            "main.xhtml",
            "<html><body><h1>Hello World</h1></body></html>",
        )];
        let container = simple_container("content.opf");
        let opf = simple_opf("Simple", &[], &chapters);
        let epub = build_epub(&container, "content.opf", &opf, &chapters);

        let result = EpubConverter.convert(&epub, &info_epub()).unwrap();

        assert!(
            result.markdown.contains("Hello World"),
            "chapter text missing: {}",
            result.markdown
        );
    }

    #[test]
    fn script_and_style_stripped() {
        let chapters = [(
            "ch.xhtml",
            "<html><head><style>body{color:red}</style></head><body><script>alert(1)</script><p>Visible text</p></body></html>",
        )];
        let container = simple_container("content.opf");
        let opf = simple_opf("Strip Test", &[], &chapters);
        let epub = build_epub(&container, "content.opf", &opf, &chapters);

        let result = EpubConverter.convert(&epub, &info_epub()).unwrap();

        assert!(
            result.markdown.contains("Visible text"),
            "visible text missing: {}",
            result.markdown
        );
        assert!(
            !result.markdown.contains("alert"),
            "script not stripped: {}",
            result.markdown
        );
        assert!(
            !result.markdown.contains("color:red"),
            "style not stripped: {}",
            result.markdown
        );
    }

    // ── OPF in subdirectory ───────────────────────────────────────────────────

    #[test]
    fn opf_in_subdirectory_resolves_hrefs() {
        let container = simple_container("OEBPS/content.opf");

        // OPF references chapters by relative href (no path prefix)
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata>
    <dc:title>Sub Book</dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="ch0" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch0"/>
  </spine>
</package>"#;

        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);

        zip.start_file("META-INF/container.xml", opts).unwrap();
        zip.write_all(container.as_bytes()).unwrap();

        zip.start_file("OEBPS/content.opf", opts).unwrap();
        zip.write_all(opf.as_bytes()).unwrap();

        // Chapter lives at OEBPS/ch1.xhtml (resolved from base "OEBPS")
        zip.start_file("OEBPS/ch1.xhtml", opts).unwrap();
        zip.write_all(b"<html><body><p>Sub-dir chapter</p></body></html>")
            .unwrap();

        let epub_bytes = zip.finish().unwrap().into_inner();

        let result = EpubConverter.convert(&epub_bytes, &info_epub()).unwrap();

        assert!(
            result.markdown.contains("Sub-dir chapter"),
            "subdirectory chapter missing: {}",
            result.markdown
        );
    }

    // ── Empty spine ───────────────────────────────────────────────────────────

    #[test]
    fn empty_spine_returns_only_metadata() {
        let container = simple_container("content.opf");
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata>
    <dc:title>Empty Book</dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest/>
  <spine/>
</package>"#;

        let epub = build_epub(&container, "content.opf", opf, &[]);

        let result = EpubConverter.convert(&epub, &info_epub()).unwrap();

        assert_eq!(result.title.as_deref(), Some("Empty Book"));
        assert!(result.markdown.contains("**Title:** Empty Book"));
    }
}
