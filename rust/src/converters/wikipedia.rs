use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;

use crate::types::{ConversionResult, Converter, StreamInfo};
use crate::utils::html_to_md::html_to_markdown;

// Mirrors TS: /^https?:\/\/[a-zA-Z]{2,3}\.wikipedia\.org\//
static WIKIPEDIA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://[a-zA-Z]{2,3}\.wikipedia\.org/").unwrap());

pub struct WikipediaConverter;

impl WikipediaConverter {
    fn do_convert(&self, input: &[u8], _info: &StreamInfo) -> Result<ConversionResult> {
        // Decode bytes — charset-aware. TS: new TextDecoder(charset || "utf-8").
        // We use lossy UTF-8; other charsets would need encoding_rs (not in deps).
        let html = String::from_utf8_lossy(input).into_owned();

        // ── Extract main content div ───────────────────────────────────────────
        // (?is): i = case-insensitive, s = dot matches newline
        // Mirrors TS: /<div[^>]*id="mw-content-text"[^>]*>([\s\S]*?)<\/div>\s*(?:<\/div>|$)/i
        // Using (?s) so bare . matches newlines; avoids [\s\S] escaping in raw strings.
        let re_content =
            Regex::new(r#"(?is)<div[^>]*id="mw-content-text"[^>]*>(.*?)</div>\s*(?:</div>|$)"#)?;
        let content_cap = re_content.captures(&html);

        // ── Extract title ─────────────────────────────────────────────────────
        // First try <span class="mw-page-title-main">, then <title>
        let re_title_span =
            Regex::new(r#"(?is)<span[^>]*class="mw-page-title-main"[^>]*>(.*?)</span>"#)?;
        let re_title_tag = Regex::new(r"(?is)<title[^>]*>(.*?)</title>")?;

        let title_raw = re_title_span
            .captures(&html)
            .map(|c| c[1].to_string())
            .or_else(|| re_title_tag.captures(&html).map(|c| c[1].to_string()));

        // TS: titleMatch[1].replace(/ - Wikipedia$/, "").trim() — end-anchored
        // suffix strip on the raw capture, then trim.
        let title = title_raw
            .map(|t| {
                t.strip_suffix(" - Wikipedia")
                    .unwrap_or(&t)
                    .trim()
                    .to_string()
            })
            .filter(|t| !t.is_empty());

        // Use extracted content section, or fall back to full HTML
        let mut content: String = match &content_cap {
            Some(m) => m[1].to_string(),
            None => html.clone(),
        };

        // ── Clean up Wikipedia-specific elements ──────────────────────────────
        // Mirrors the TS replace chain exactly: same patterns, same order.
        // [\s\S] in raw strings: use [^\x00]* trick or rely on (?s) + .*
        // We use (?s) flag so .* matches newlines too.
        let re_script = Regex::new(r"(?is)<script.*?</script>")?;
        let re_style = Regex::new(r"(?is)<style.*?</style>")?;
        // TS: /<div[^>]*class="[^"]*mw-editsection[^"]*"[\s\S]*?<\/div>/gi
        let re_editsection =
            Regex::new(r#"(?is)<div[^>]*class="[^"]*mw-editsection[^"]*".*?</div>"#)?;
        // TS: /<sup[^>]*class="[^"]*reference[^"]*"[\s\S]*?<\/sup>/gi
        let re_reference = Regex::new(r#"(?is)<sup[^>]*class="[^"]*reference[^"]*".*?</sup>"#)?;
        // TS: /<div[^>]*class="[^"]*navbox[\s\S]*?<\/div>/gi
        let re_navbox = Regex::new(r#"(?is)<div[^>]*class="[^"]*navbox.*?</div>"#)?;
        // TS: /<table[^>]*class="[^"]*sidebar[\s\S]*?<\/table>/gi
        let re_sidebar = Regex::new(r#"(?is)<table[^>]*class="[^"]*sidebar.*?</table>"#)?;

        content = re_script.replace_all(&content, "").into_owned();
        content = re_style.replace_all(&content, "").into_owned();
        content = re_editsection.replace_all(&content, "").into_owned();
        content = re_reference.replace_all(&content, "").into_owned();
        content = re_navbox.replace_all(&content, "").into_owned();
        content = re_sidebar.replace_all(&content, "").into_owned();

        // ── Convert HTML → Markdown ───────────────────────────────────────────
        let markdown = html_to_markdown(&content).trim().to_string();
        let result = if let Some(ref t) = title {
            format!("# {t}\n\n{markdown}")
        } else {
            markdown
        };

        Ok(ConversionResult {
            markdown: result,
            title,
        })
    }
}

impl Converter for WikipediaConverter {
    fn name(&self) -> &'static str {
        "wikipedia"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        let Some(url) = &info.url else {
            return false;
        };
        WIKIPEDIA_RE.is_match(url)
    }

    fn convert(&self, input: &[u8], info: &StreamInfo) -> Result<ConversionResult> {
        self.do_convert(input, info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_with_url(url: &str) -> StreamInfo {
        StreamInfo {
            url: Some(url.to_string()),
            ..Default::default()
        }
    }

    // ── accepts() ─────────────────────────────────────────────────────────────

    #[test]
    fn accepts_english_wikipedia() {
        let c = WikipediaConverter;
        assert!(c.accepts(&info_with_url("https://en.wikipedia.org/wiki/Rust")));
    }

    #[test]
    fn accepts_two_letter_lang_code() {
        let c = WikipediaConverter;
        assert!(c.accepts(&info_with_url("https://de.wikipedia.org/wiki/Rust")));
    }

    #[test]
    fn accepts_three_letter_lang_code() {
        let c = WikipediaConverter;
        assert!(c.accepts(&info_with_url("https://als.wikipedia.org/wiki/Rust")));
    }

    #[test]
    fn accepts_http_scheme() {
        let c = WikipediaConverter;
        assert!(c.accepts(&info_with_url("http://en.wikipedia.org/wiki/Rust")));
    }

    #[test]
    fn rejects_non_wikipedia_url() {
        let c = WikipediaConverter;
        assert!(!c.accepts(&info_with_url("https://example.com/page")));
    }

    #[test]
    fn rejects_when_no_url() {
        let c = WikipediaConverter;
        assert!(!c.accepts(&StreamInfo {
            extension: Some(".html".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn rejects_wikipedia_org_without_lang_subdomain() {
        // Pattern requires [a-zA-Z]{2,3} before .wikipedia.org
        let c = WikipediaConverter;
        assert!(!c.accepts(&info_with_url("https://wikipedia.org/wiki/Rust")));
    }

    // ── convert(): title extraction ───────────────────────────────────────────

    #[test]
    fn extracts_title_from_mw_page_title_main_span() {
        let c = WikipediaConverter;
        let html = r#"<html><head><title>Rust programming language - Wikipedia</title></head>
<body>
<span class="mw-page-title-main">Rust (programming language)</span>
<div id="mw-content-text"><p>Rust is a systems programming language.</p></div>
</body></html>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/Rust"),
            )
            .unwrap();
        assert_eq!(result.title.as_deref(), Some("Rust (programming language)"));
        assert!(result.markdown.starts_with("# Rust (programming language)"));
    }

    #[test]
    fn falls_back_to_title_tag_and_strips_wikipedia_suffix() {
        let c = WikipediaConverter;
        let html = r#"<html><head><title>Rust - Wikipedia</title></head>
<body><div id="mw-content-text"><p>Hello.</p></div></body></html>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/Rust"),
            )
            .unwrap();
        assert_eq!(result.title.as_deref(), Some("Rust"));
    }

    #[test]
    fn title_without_wikipedia_suffix_is_kept_as_is() {
        let c = WikipediaConverter;
        let html = r#"<html><head><title>My Custom Title</title></head>
<body><p>Content.</p></body></html>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/X"),
            )
            .unwrap();
        assert_eq!(result.title.as_deref(), Some("My Custom Title"));
    }

    // ── convert(): cleanup ────────────────────────────────────────────────────

    #[test]
    fn removes_script_tags() {
        let c = WikipediaConverter;
        let html = r#"<p>Hello</p><script type="text/javascript">var x = 1;</script><p>World</p>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/X"),
            )
            .unwrap();
        assert!(
            !result.markdown.contains("var x"),
            "scripts should be removed"
        );
        assert!(result.markdown.contains("Hello"), "prose should remain");
    }

    #[test]
    fn removes_style_tags() {
        let c = WikipediaConverter;
        let html = r#"<p>Text</p><style>.foo { color: red; }</style><p>More</p>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/X"),
            )
            .unwrap();
        assert!(
            !result.markdown.contains("color: red"),
            "styles should be removed"
        );
    }

    #[test]
    fn removes_edit_section_links() {
        let c = WikipediaConverter;
        let html = r#"<h2>History</h2><div class="mw-editsection">[<a href="/w/index.php?action=edit">edit</a>]</div><p>Some history.</p>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/X"),
            )
            .unwrap();
        assert!(
            !result.markdown.contains("edit"),
            "edit sections should be removed: {}",
            result.markdown
        );
    }

    #[test]
    fn removes_reference_superscripts() {
        let c = WikipediaConverter;
        let html = r#"<p>This is a fact.<sup class="reference">[1]</sup></p>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/X"),
            )
            .unwrap();
        assert!(
            !result.markdown.contains("[1]"),
            "references should be removed: {}",
            result.markdown
        );
        assert!(result.markdown.contains("fact"), "prose should remain");
    }

    #[test]
    fn removes_navboxes() {
        let c = WikipediaConverter;
        let html = r#"<p>Article content.</p><div class="navbox">Navigation links here</div>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/X"),
            )
            .unwrap();
        assert!(
            !result.markdown.contains("Navigation links"),
            "navboxes should be removed"
        );
        assert!(
            result.markdown.contains("Article content"),
            "prose should remain"
        );
    }

    #[test]
    fn removes_sidebar_tables() {
        let c = WikipediaConverter;
        let html =
            r#"<table class="sidebar"><tr><td>Sidebar info</td></tr></table><p>Main content.</p>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/X"),
            )
            .unwrap();
        assert!(
            !result.markdown.contains("Sidebar info"),
            "sidebars should be removed"
        );
        assert!(
            result.markdown.contains("Main content"),
            "prose should remain"
        );
    }

    // ── convert(): full round-trip ────────────────────────────────────────────

    #[test]
    fn produces_heading_and_prose() {
        let c = WikipediaConverter;
        let html = r#"<html><head><title>Ferris - Wikipedia</title></head>
<body>
<span class="mw-page-title-main">Ferris the Crab</span>
<div id="mw-content-text">
  <p>Ferris is the mascot of Rust.</p>
  <p>Everyone loves Ferris.</p>
</div>
</body></html>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/Ferris"),
            )
            .unwrap();
        assert!(
            result.markdown.starts_with("# Ferris the Crab"),
            "got: {}",
            result.markdown
        );
        assert!(
            result.markdown.contains("Ferris is the mascot"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn handles_missing_content_div_by_using_full_html() {
        let c = WikipediaConverter;
        let html = r#"<p>Just a paragraph with no content div.</p>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/X"),
            )
            .unwrap();
        assert!(
            result.markdown.contains("Just a paragraph"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn result_markdown_starts_with_title_heading_when_title_present() {
        let c = WikipediaConverter;
        let html = r#"<html><head><title>Test - Wikipedia</title></head>
<body><p>Body text.</p></body></html>"#;
        let result = c
            .convert(
                html.as_bytes(),
                &info_with_url("https://en.wikipedia.org/wiki/Test"),
            )
            .unwrap();
        assert_eq!(
            result.markdown.lines().next(),
            Some("# Test"),
            "first line should be '# Test', got: {}",
            result.markdown
        );
    }
}
