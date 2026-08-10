use anyhow::Result;
use regex::Regex;

use super::decode_text;
use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};
use crate::utils::html_to_md::{html_to_markdown, normalize_tables_html};

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

    fn convert(
        &self,
        input: &[u8],
        _info: &StreamInfo,
        _options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let html = decode_text(input);

        // Remove script and style tags before converting
        let re_script = Regex::new(r"(?is)<script[\s\S]*?</script>").unwrap();
        let re_style = Regex::new(r"(?is)<style[\s\S]*?</style>").unwrap();
        let cleaned = re_script.replace_all(&html, "");
        let cleaned = re_style.replace_all(&cleaned, "");

        let normalized = normalize_tables_html(&cleaned);
        let markdown = html_to_markdown(&normalized);

        // Extract title
        let re_title = Regex::new(r"(?is)<title[^>]*>([\s\S]*?)</title>").unwrap();
        let title = re_title
            .captures(&html)
            .map(|c| c[1].trim().to_string())
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
        let result = HtmlConverter
            .convert(html, &info(".html"), &MarkitOptions::default())
            .unwrap();
        assert_eq!(result.markdown, "# Hello\n\nWorld");
        assert_eq!(result.title, None);
    }

    #[test]
    fn test_strips_script_and_style() {
        let html = b"<style>body { color: red; }</style><script>alert(1);</script><p>Hello</p>";
        let result = HtmlConverter
            .convert(html, &info(".html"), &MarkitOptions::default())
            .unwrap();
        assert_eq!(result.markdown, "Hello");
    }

    #[test]
    fn test_extracts_title() {
        let html = b"<html><head><title>My Page</title></head><body><p>Content</p></body></html>";
        let result = HtmlConverter
            .convert(html, &info(".html"), &MarkitOptions::default())
            .unwrap();
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
            .convert(html.as_bytes(), &info(".html"), &MarkitOptions::default())
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
        let result = HtmlConverter
            .convert(html, &info(".html"), &MarkitOptions::default())
            .unwrap();
        assert_eq!(
            result.markdown,
            "| Name | Age |\n| --- | --- |\n| Alice | 30 |"
        );
    }
}
