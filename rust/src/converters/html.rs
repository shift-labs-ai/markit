use anyhow::Result;

use super::decode_text;
use crate::types::{ConversionResult, Converter, StreamInfo};
use crate::utils::html_to_md::{html_to_markdown, normalize_tables_html};
use crate::utils::strip_blocks::{first_tag_content, strip_tag_blocks};

const EXTENSIONS: &[&str] = &[".html", ".htm"];
const MIMETYPES: &[&str] = &["text/html", "application/xhtml"];

pub struct HtmlConverter;

impl Converter for HtmlConverter {
    fn name(&self) -> &'static str {
        "html"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ref ext) = info.extension {
            if EXTENSIONS.contains(&ext.as_str()) {
                return true;
            }
        }
        if let Some(ref mime) = info.mimetype {
            if MIMETYPES.iter().any(|m| mime.starts_with(m)) {
                return true;
            }
        }
        false
    }

    fn convert(&self, input: &[u8], _info: &StreamInfo) -> Result<ConversionResult> {
        let html = decode_text(input);

        // Remove script and style tags before converting
        let cleaned = strip_tag_blocks(&html, "<script", "</script>");
        let cleaned = strip_tag_blocks(&cleaned, "<style", "</style>");

        let normalized = normalize_tables_html(&cleaned);
        let markdown = html_to_markdown(&normalized);

        // Extract title
        let title = first_tag_content(&html, "<title", "</title>")
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());

        Ok(ConversionResult {
            markdown: markdown.trim().to_string(),
            title,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(ext: &str) -> StreamInfo {
        StreamInfo {
            extension: Some(ext.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_accepts() {
        let c = HtmlConverter;
        assert!(c.accepts(&info(".html")));
        assert!(c.accepts(&info(".htm")));
        assert!(!c.accepts(&info(".txt")));
        assert!(c.accepts(&StreamInfo {
            mimetype: Some("text/html".to_string()),
            ..Default::default()
        }));
        assert!(c.accepts(&StreamInfo {
            mimetype: Some("application/xhtml+xml".to_string()),
            ..Default::default()
        }));
    }

    #[test]
    fn test_basic_conversion() {
        let html = b"<h1>Hello</h1><p>World</p>";
        let result = HtmlConverter.convert(html, &info(".html")).unwrap();
        assert_eq!(result.markdown, "# Hello\n\nWorld");
        assert_eq!(result.title, None);
    }

    #[test]
    fn test_strips_script_and_style() {
        let html = b"<style>body { color: red; }</style><script>alert(1);</script><p>Hello</p>";
        let result = HtmlConverter.convert(html, &info(".html")).unwrap();
        assert_eq!(result.markdown, "Hello");
    }

    #[test]
    fn test_extracts_title() {
        let html = b"<html><head><title>My Page</title></head><body><p>Content</p></body></html>";
        let result = HtmlConverter.convert(html, &info(".html")).unwrap();
        assert_eq!(result.title, Some("My Page".to_string()));
        assert!(result.markdown.contains("Content"));
    }

    #[test]
    fn test_full_html_doc() {
        let html = concat!(
            "<!DOCTYPE html><html><head><title>My Page Title</title>",
            "<style>body { color: red; }</style>",
            "<script>alert('hi');</script>",
            "</head><body>",
            "<h1>Welcome</h1>",
            "<p>Hello world</p>",
            "</body></html>"
        );
        let result = HtmlConverter
            .convert(html.as_bytes(), &info(".html"))
            .unwrap();
        assert_eq!(result.title, Some("My Page Title".to_string()));
        // turndown keeps <title> text in the body output — the TS CLI does too,
        // and the Rust engine replicates that quirk for byte parity.
        assert_eq!(result.markdown, "My Page Title\n\n# Welcome\n\nHello world");
    }

    #[test]
    fn test_table_normalization() {
        let html =
            b"<table><tr><td>Name</td><td>Age</td></tr><tr><td>Alice</td><td>30</td></tr></table>";
        let result = HtmlConverter.convert(html, &info(".html")).unwrap();
        assert_eq!(
            result.markdown,
            "| Name | Age |\n| --- | --- |\n| Alice | 30 |"
        );
    }
}
