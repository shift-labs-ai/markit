//! Port of `discoverMarkdownSource` from src/markit.ts.
//!
//! Discovers a raw markdown source URL from an HTML page by inspecting:
//! 1. `<link rel="alternate" type="text/markdown" href="...">` tags
//! 2. VitePress markers (`__VP_HASH_MAP__`, `VPContent`, `"vitepress"`) — only when ext is empty

use regex::Regex;
use std::sync::OnceLock;

// Regex form 1: rel before type
fn link_regex_1() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)<link[^>]+rel=["']alternate["'][^>]+type=["']text/markdown["'][^>]+href=["']([^"']+)["']"#,
        )
        .unwrap()
    })
}

// Regex form 2: type before rel
fn link_regex_2() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)<link[^>]+type=["']text/markdown["'][^>]+rel=["']alternate["'][^>]+href=["']([^"']+)["']"#,
        )
        .unwrap()
    })
}

/// Minimal URL resolver — no new deps.
///
/// Rules (mirrors browser URL resolution):
/// - Absolute `http(s)://` hrefs pass through unchanged.
/// - `//host/...` → scheme from base + the href.
/// - `/path` → origin from base + path.
/// - Relative (`./path` or `path`) → resolve against the directory of the base path.
fn resolve_url(href: &str, base: &str) -> Option<String> {
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_string());
    }

    // Extract scheme from base URL
    let (scheme, rest) = if let Some(s) = base.strip_prefix("https://") {
        ("https", s)
    } else if let Some(s) = base.strip_prefix("http://") {
        ("http", s)
    } else {
        return None;
    };

    if href.starts_with("//") {
        return Some(format!("{}:{}", scheme, href));
    }

    // Extract origin (scheme + authority/host)
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..host_end];
    let origin = format!("{}://{}", scheme, host);

    if href.starts_with('/') {
        return Some(format!("{}{}", origin, href));
    }

    // Relative URL — resolve against the directory part of the base path.
    // e.g. base path `/docs/page`, dir = `/docs/`, href `./page.md` → `/docs/page.md`
    let path_part = &rest[host_end..]; // e.g. "/docs/page"
    let dir = if let Some(idx) = path_part.rfind('/') {
        &path_part[..=idx]
    } else {
        "/"
    };

    // Strip leading "./" from relative href
    let href_clean = href.strip_prefix("./").unwrap_or(href);

    Some(format!("{}{}{}", origin, dir, href_clean))
}

/// Strip trailing slash then append `.md`.
fn append_md_extension(url: &str) -> String {
    if url.ends_with('/') {
        format!("{}.md", &url[..url.len() - 1])
    } else {
        format!("{}.md", url)
    }
}

/// Discover a raw markdown source URL from an HTML page.
///
/// Returns the resolved absolute URL of the markdown source, or `None` if
/// no discoverable source is found.
///
/// Mirrors `discoverMarkdownSource` exported from src/markit.ts.
#[allow(dead_code)]
pub fn discover_markdown_source(html: &str, url: &str, ext: &str) -> Option<String> {
    // 1. Look for <link rel="alternate" type="text/markdown" href="...">
    //    Two regex forms handle attribute order variations.
    let href = link_regex_1()
        .captures(html)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        .or_else(|| {
            link_regex_2()
                .captures(html)
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        });

    if let Some(h) = href {
        if let Some(resolved) = resolve_url(&h, url) {
            return Some(resolved);
        }
    }

    // 2. VitePress detection — serves .md alongside HTML.
    //    Only applicable when the URL has no file extension (ext is empty).
    if ext.is_empty()
        && (html.contains("__VP_HASH_MAP__")
            || html.contains("VPContent")
            || html.contains("vitepress"))
    {
        return Some(append_md_extension(url));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── link rel="alternate" ──────────────────────────────────────────

    #[test]
    fn finds_link_rel_alternate_type_text_markdown_with_absolute_href() {
        let html = r#"<html><head><link rel="alternate" type="text/markdown" href="https://example.com/post.md"></head></html>"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/post", ""),
            Some("https://example.com/post.md".to_string())
        );
    }

    #[test]
    fn finds_link_with_attributes_in_reverse_order_type_before_rel() {
        let html = r#"<link type="text/markdown" rel="alternate" href="/docs/page.md">"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/docs/page", ""),
            Some("https://example.com/docs/page.md".to_string())
        );
    }

    #[test]
    fn resolves_relative_href_against_the_page_url() {
        let html = r#"<link rel="alternate" type="text/markdown" href="./page.md">"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/docs/page", ""),
            Some("https://example.com/docs/page.md".to_string())
        );
    }

    #[test]
    fn resolves_root_relative_href() {
        let html = r#"<link rel="alternate" type="text/markdown" href="/blog/post.md">"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/blog/post", ""),
            Some("https://example.com/blog/post.md".to_string())
        );
    }

    #[test]
    fn handles_single_quotes_in_link_tag() {
        let html = r#"<link rel='alternate' type='text/markdown' href='/page.md'>"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/page", ""),
            Some("https://example.com/page.md".to_string())
        );
    }

    #[test]
    fn link_alternate_takes_priority_over_vitepress_markers() {
        let html = r#"<head><link rel="alternate" type="text/markdown" href="/custom-source.md"></head><div id="VPContent">vitepress</div>"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/docs/page", ""),
            Some("https://example.com/custom-source.md".to_string())
        );
    }

    #[test]
    fn ignores_link_alternate_with_wrong_type() {
        let html = r#"<link rel="alternate" type="application/rss+xml" href="/feed.xml">"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/page", ""),
            None
        );
    }

    #[test]
    fn ignores_link_alternate_with_empty_href() {
        // empty href doesn't match — regex requires at least one char
        let html = r#"<link rel="alternate" type="text/markdown" href="">"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/page", ""),
            None
        );
    }

    // ── VitePress detection ───────────────────────────────────────────

    #[test]
    fn detects_vitepress_via_vp_hash_map() {
        let html = r#"<script>window.__VP_HASH_MAP__=JSON.parse("{}")</script>"#;
        assert_eq!(
            discover_markdown_source(html, "https://docs.example.com/guide/intro", ""),
            Some("https://docs.example.com/guide/intro.md".to_string())
        );
    }

    #[test]
    fn detects_vitepress_via_vpcontent() {
        let html = r#"<div id="VPContent"><main>...</main></div>"#;
        assert_eq!(
            discover_markdown_source(html, "https://docs.example.com/guide/intro", ""),
            Some("https://docs.example.com/guide/intro.md".to_string())
        );
    }

    #[test]
    fn detects_vitepress_via_vitepress_string_in_html() {
        let html = r#"<meta name="generator" content="vitepress">"#;
        assert_eq!(
            discover_markdown_source(html, "https://docs.example.com/api/config", ""),
            Some("https://docs.example.com/api/config.md".to_string())
        );
    }

    #[test]
    fn strips_trailing_slash_before_appending_md_for_vitepress() {
        let html = r#"<div id="VPContent"></div>"#;
        assert_eq!(
            discover_markdown_source(html, "https://docs.example.com/guide/intro/", ""),
            Some("https://docs.example.com/guide/intro.md".to_string())
        );
    }

    #[test]
    fn does_not_detect_vitepress_when_url_has_an_extension() {
        let html = r#"<div id="VPContent"></div>"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/page.html", ".html"),
            None
        );
    }

    // ── No match ──────────────────────────────────────────────────────

    #[test]
    fn returns_null_for_plain_html_with_no_markers() {
        let html = r#"<html><body><h1>Hello</h1></body></html>"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/page", ""),
            None
        );
    }

    #[test]
    fn returns_null_for_empty_html() {
        assert_eq!(
            discover_markdown_source("", "https://example.com/page", ""),
            None
        );
    }

    #[test]
    fn returns_null_when_url_has_extension_even_with_no_markers() {
        let html = r#"<html><body>plain</body></html>"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/file.pdf", ".pdf"),
            None
        );
    }

    // ── Edge cases ────────────────────────────────────────────────────

    #[test]
    fn handles_url_with_query_string_vitepress() {
        let html = r#"<div id="VPContent"></div>"#;
        assert_eq!(
            discover_markdown_source(
                html,
                "https://docs.example.com/guide/intro?ref=nav",
                ""
            ),
            Some("https://docs.example.com/guide/intro?ref=nav.md".to_string())
        );
    }

    #[test]
    fn handles_url_with_hash_fragment_vitepress() {
        let html = r#"<div id="VPContent"></div>"#;
        // extname won't pick up fragment, so ext is ""
        assert_eq!(
            discover_markdown_source(
                html,
                "https://docs.example.com/guide/intro#section",
                ""
            ),
            Some("https://docs.example.com/guide/intro#section.md".to_string())
        );
    }

    #[test]
    fn vitepress_marker_buried_deep_in_large_html_still_matches() {
        let padding = "<div>content</div>".repeat(500);
        let html = format!(
            "<html>{}<script>window.__VP_HASH_MAP__={{}}</script></html>",
            padding
        );
        assert_eq!(
            discover_markdown_source(&html, "https://example.com/docs/big-page", ""),
            Some("https://example.com/docs/big-page.md".to_string())
        );
    }

    #[test]
    fn multiple_link_alternates_first_text_markdown_wins() {
        let html = r#"
      <link rel="alternate" type="application/rss+xml" href="/feed.xml">
      <link rel="alternate" type="text/markdown" href="/first.md">
      <link rel="alternate" type="text/markdown" href="/second.md">
    "#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/page", ""),
            Some("https://example.com/first.md".to_string())
        );
    }

    #[test]
    fn case_insensitive_matching_for_link_tag() {
        let html = r#"<LINK REL="alternate" TYPE="text/markdown" HREF="/page.md">"#;
        assert_eq!(
            discover_markdown_source(html, "https://example.com/page", ""),
            Some("https://example.com/page.md".to_string())
        );
    }
}
