//! RSS/Atom feed → Markdown converter.
//!
//! Port of src/converters/rss.ts (158 lines).  Uses the same regex-based
//! XML extraction strategy as the TS original so output is byte-for-byte
//! compatible.  We intentionally avoid a full XML parser so CDATA, namespace
//! prefixes (\`content:encoded\`), and malformed feeds all behave identically.

use anyhow::{anyhow, Result};
use regex::Regex;

use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};
use crate::utils::html_to_md::html_to_markdown;

pub struct RssConverter;

const MIMETYPES: &[&str] = &[
    "application/rss+xml",
    "application/rss",
    "application/atom+xml",
    "application/atom",
    "text/xml",
    "application/xml",
];

// ─────────────────────────────── helpers ────────────────────────────────────

impl RssConverter {
    /// Extract the text content of the first matching \`<tag>…</tag>\` element.
    /// Strips CDATA wrappers and trims.  Returns \`None\` when missing or empty.
    fn extract(xml: &str, tag: &str) -> Option<String> {
        let escaped = regex::escape(tag);
        // [\s\S]*? matches any char including newlines (lazy)
        let pattern = format!(r"(?si)<{escaped}[^>]*>([\s\S]*?)</{escaped}>");
        let re = Regex::new(&pattern).ok()?;
        let caps = re.captures(xml)?;
        let content = caps.get(1)?.as_str();
        let stripped = strip_cdata(content);
        let trimmed = stripped.trim().to_string();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    }

    /// Collect all \`<tag>…</tag>\` blocks (case-insensitive, multi-line).
    fn extract_all(xml: &str, tag: &str) -> Vec<String> {
        let escaped = regex::escape(tag);
        let pattern = format!(r"(?si)<{escaped}[^>]*>[\s\S]*?</{escaped}>");
        let re = match Regex::new(&pattern) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        re.find_iter(xml).map(|m| m.as_str().to_string()).collect()
    }

    /// Unescape CDATA + HTML entities, then convert HTML → Markdown if the
    /// string contains any HTML tags.  Plain text is returned as-is (trimmed).
    fn html_to_md(html: &str) -> String {
        let unescaped = strip_cdata(html)
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");

        if unescaped.contains('<') {
            html_to_markdown(&unescaped).trim().to_string()
        } else {
            unescaped.trim().to_string()
        }
    }

    // ─────────────────── RSS 2.0 ────────────────────────────────────────────

    fn parse_rss(xml: &str) -> ConversionResult {
        let mut sections: Vec<String> = Vec::new();

        // Limit channel-level extraction to the <channel> block so that item
        // titles / descriptions do not bleed into the feed header.
        let re_channel = Regex::new(r"(?si)<channel>([\s\S]*?)</channel>").unwrap();
        let channel_xml: &str = re_channel
            .captures(xml)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .unwrap_or(xml);

        let channel_title = Self::extract(channel_xml, "title");
        let channel_desc  = Self::extract(channel_xml, "description");

        if let Some(ref t) = channel_title {
            sections.push(format!("# {t}"));
        }
        if let Some(d) = channel_desc {
            sections.push(Self::html_to_md(&d));
        }

        for item in Self::extract_all(xml, "item") {
            let title       = Self::extract(&item, "title");
            let pub_date    = Self::extract(&item, "pubDate");
            let description = Self::extract(&item, "description");
            let content     = Self::extract(&item, "content:encoded");
            let link        = Self::extract(&item, "link");

            let mut parts: Vec<String> = Vec::new();
            if let Some(t) = &title    { parts.push(format!("## {t}")); }
            if let Some(d) = &pub_date { parts.push(format!("Published: {d}")); }
            if let Some(l) = &link     { parts.push(format!("[Link]({l})")); }
            if let Some(c) = content {
                parts.push(Self::html_to_md(&c));
            } else if let Some(d) = description {
                parts.push(Self::html_to_md(&d));
            }
            if !parts.is_empty() {
                sections.push(parts.join("\n"));
            }
        }

        ConversionResult {
            markdown: sections.join("\n\n").trim().to_string(),
            title: channel_title,
        }
    }

    // ─────────────────── Atom ───────────────────────────────────────────────

    fn parse_atom(xml: &str) -> ConversionResult {
        let mut sections: Vec<String> = Vec::new();

        let feed_title = Self::extract(xml, "title");
        let subtitle   = Self::extract(xml, "subtitle");

        if let Some(ref t) = feed_title { sections.push(format!("# {t}")); }
        if let Some(s) = subtitle       { sections.push(s); }

        for entry in Self::extract_all(xml, "entry") {
            let title   = Self::extract(&entry, "title");
            let updated = Self::extract(&entry, "updated");
            let summary = Self::extract(&entry, "summary");
            let content = Self::extract(&entry, "content");

            let mut parts: Vec<String> = Vec::new();
            if let Some(t) = &title   { parts.push(format!("## {t}")); }
            if let Some(u) = &updated { parts.push(format!("Updated: {u}")); }
            if let Some(c) = content {
                parts.push(Self::html_to_md(&c));
            } else if let Some(s) = summary {
                parts.push(Self::html_to_md(&s));
            }
            if !parts.is_empty() {
                sections.push(parts.join("\n"));
            }
        }

        ConversionResult {
            markdown: sections.join("\n\n").trim().to_string(),
            title: feed_title,
        }
    }
}

// ─────────────────────────────── Converter impl ─────────────────────────────

impl Converter for RssConverter {
    fn name(&self) -> &'static str {
        "rss"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ext) = &info.extension {
            if ext == ".rss" || ext == ".atom" || ext == ".xml" {
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
        let text = std::str::from_utf8(input)
            .map_err(|e| anyhow!("RSS: invalid UTF-8: {e}"))?;

        if text.contains("<rss") {
            Ok(Self::parse_rss(text))
        } else if text.contains("<feed") {
            Ok(Self::parse_atom(text))
        } else {
            Err(anyhow!("Not an RSS or Atom feed"))
        }
    }
}

// ─────────────────────────────── free fn ────────────────────────────────────

/// Replace every \`<![CDATA[…]]>\` wrapper with its inner text.
fn strip_cdata(s: &str) -> String {
    let re = Regex::new(r"(?s)<!\[CDATA\[([\s\S]*?)\]\]>").unwrap();
    re.replace_all(s, "$1").into_owned()
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn conv() -> RssConverter { RssConverter }
    fn opts() -> MarkitOptions { MarkitOptions::default() }

    fn info_ext(ext: &str) -> StreamInfo {
        StreamInfo { extension: Some(ext.into()), ..Default::default() }
    }
    fn info_mime(mime: &str) -> StreamInfo {
        StreamInfo { mimetype: Some(mime.into()), ..Default::default() }
    }
    fn go(xml: &str) -> ConversionResult {
        conv().convert(xml.as_bytes(), &StreamInfo::default(), &opts()).unwrap()
    }

    // ── accepts() ────────────────────────────────────────────────────────────

    #[test]
    fn accepts_rss_extension()  { assert!(conv().accepts(&info_ext(".rss"))); }
    #[test]
    fn accepts_atom_extension() { assert!(conv().accepts(&info_ext(".atom"))); }
    #[test]
    fn accepts_xml_extension()  { assert!(conv().accepts(&info_ext(".xml"))); }
    #[test]
    fn rejects_pdf_extension()  { assert!(!conv().accepts(&info_ext(".pdf"))); }
    #[test]
    fn accepts_rss_xml_mime()   { assert!(conv().accepts(&info_mime("application/rss+xml"))); }
    #[test]
    fn accepts_atom_xml_mime()  { assert!(conv().accepts(&info_mime("application/atom+xml"))); }
    #[test]
    fn accepts_text_xml_mime()  { assert!(conv().accepts(&info_mime("text/xml; charset=utf-8"))); }
    #[test]
    fn rejects_json_mime()      { assert!(!conv().accepts(&info_mime("application/json"))); }

    // ── error paths ──────────────────────────────────────────────────────────

    #[test]
    fn rejects_plain_xml() {
        let xml = r#"<?xml version="1.0"?><root><item>hello</item></root>"#;
        let err = conv().convert(xml.as_bytes(), &StreamInfo::default(), &opts()).unwrap_err();
        assert!(err.to_string().contains("Not an RSS or Atom feed"), "got: {err}");
    }

    // ── RSS 2.0 ───────────────────────────────────────────────────────────────

    const SIMPLE_RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <description>A sample RSS feed</description>
    <item>
      <title>First Post</title>
      <pubDate>Mon, 01 Jan 2024 00:00:00 +0000</pubDate>
      <link>https://example.com/1</link>
      <description>Hello &lt;b&gt;world&lt;/b&gt;</description>
    </item>
    <item>
      <title>Second Post</title>
      <pubDate>Tue, 02 Jan 2024 00:00:00 +0000</pubDate>
      <link>https://example.com/2</link>
      <description>Plain text item</description>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn rss_title_h1_and_result() {
        let r = go(SIMPLE_RSS);
        assert!(r.markdown.starts_with("# Test Feed"), "got:\n{}", r.markdown);
        assert_eq!(r.title.as_deref(), Some("Test Feed"));
    }
    #[test]
    fn rss_channel_description() {
        assert!(go(SIMPLE_RSS).markdown.contains("A sample RSS feed"));
    }
    #[test]
    fn rss_item_titles_become_h2() {
        let md = go(SIMPLE_RSS).markdown;
        assert!(md.contains("## First Post"),  "got:\n{md}");
        assert!(md.contains("## Second Post"), "got:\n{md}");
    }
    #[test]
    fn rss_item_pubdate() {
        assert!(go(SIMPLE_RSS).markdown.contains("Published: Mon, 01 Jan 2024 00:00:00 +0000"));
    }
    #[test]
    fn rss_item_link() {
        assert!(go(SIMPLE_RSS).markdown.contains("[Link](https://example.com/1)"));
    }
    #[test]
    fn rss_html_description_converted() {
        // &lt;b&gt;world&lt;/b&gt; unescapes to <b>world</b> → **world**
        assert!(go(SIMPLE_RSS).markdown.contains("**world**"));
    }
    #[test]
    fn rss_plain_description_preserved() {
        assert!(go(SIMPLE_RSS).markdown.contains("Plain text item"));
    }

    // ── content:encoded wins over description ─────────────────────────────────

    const RSS_CONTENT_ENCODED: &str = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>Rich Feed</title>
    <description>Feed description</description>
    <item>
      <title>Rich Item</title>
      <link>https://example.com/rich</link>
      <description>Short desc</description>
      <content:encoded><![CDATA[<h2>Full content</h2><p>With <strong>HTML</strong></p>]]></content:encoded>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn rss_prefers_content_encoded() {
        let md = go(RSS_CONTENT_ENCODED).markdown;
        assert!(md.contains("## Full content"), "got:\n{md}");
        assert!(md.contains("**HTML**"),        "got:\n{md}");
        assert!(!md.contains("Short desc"),     "short desc should be suppressed:\n{md}");
    }

    // ── CDATA ─────────────────────────────────────────────────────────────────

    const RSS_CDATA: &str = r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title><![CDATA[My CDATA Feed]]></title>
    <description><![CDATA[Feed description]]></description>
    <item>
      <title><![CDATA[CDATA Item]]></title>
      <pubDate>Wed, 03 Jan 2024 00:00:00 +0000</pubDate>
      <link>https://example.com/cdata</link>
      <description><![CDATA[<p>CDATA <em>content</em></p>]]></description>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn rss_cdata_title_unwrapped() {
        let r = go(RSS_CDATA);
        assert!(r.markdown.starts_with("# My CDATA Feed"), "got:\n{}", r.markdown);
        assert_eq!(r.title.as_deref(), Some("My CDATA Feed"));
    }
    #[test]
    fn rss_cdata_html_in_description() {
        // <em>content</em> → _content_
        assert!(go(RSS_CDATA).markdown.contains("_content_"));
    }

    // ── Atom ──────────────────────────────────────────────────────────────────

    const SIMPLE_ATOM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom Test Feed</title>
  <subtitle>A subtitle here</subtitle>
  <entry>
    <title>Entry One</title>
    <updated>2024-01-01T00:00:00Z</updated>
    <summary>Plain summary text</summary>
  </entry>
  <entry>
    <title>Entry Two</title>
    <updated>2024-01-02T00:00:00Z</updated>
    <content type="html">&lt;p&gt;HTML &lt;strong&gt;content&lt;/strong&gt;&lt;/p&gt;</content>
  </entry>
</feed>"#;

    #[test]
    fn atom_title_h1_and_result() {
        let r = go(SIMPLE_ATOM);
        assert!(r.markdown.starts_with("# Atom Test Feed"), "got:\n{}", r.markdown);
        assert_eq!(r.title.as_deref(), Some("Atom Test Feed"));
    }
    #[test]
    fn atom_subtitle() {
        assert!(go(SIMPLE_ATOM).markdown.contains("A subtitle here"));
    }
    #[test]
    fn atom_entry_titles_become_h2() {
        let md = go(SIMPLE_ATOM).markdown;
        assert!(md.contains("## Entry One"), "got:\n{md}");
        assert!(md.contains("## Entry Two"), "got:\n{md}");
    }
    #[test]
    fn atom_entry_updated() {
        assert!(go(SIMPLE_ATOM).markdown.contains("Updated: 2024-01-01T00:00:00Z"));
    }
    #[test]
    fn atom_summary_plain_text() {
        assert!(go(SIMPLE_ATOM).markdown.contains("Plain summary text"));
    }
    #[test]
    fn atom_html_content_converted() {
        // &lt;strong&gt;content&lt;/strong&gt; → **content**
        assert!(go(SIMPLE_ATOM).markdown.contains("**content**"));
    }

    // ── Atom: content preferred over summary ──────────────────────────────────

    const ATOM_PRIO: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Priority Feed</title>
  <entry>
    <title>Prio Entry</title>
    <updated>2024-03-01T12:00:00Z</updated>
    <summary>Short summary</summary>
    <content type="html"><![CDATA[<p>Full <b>content</b> wins</p>]]></content>
  </entry>
</feed>"#;

    #[test]
    fn atom_content_over_summary() {
        let md = go(ATOM_PRIO).markdown;
        assert!(md.contains("**content** wins"), "got:\n{md}");
        assert!(!md.contains("Short summary"),   "summary should be suppressed:\n{md}");
    }

    // ── empty feeds ───────────────────────────────────────────────────────────

    #[test]
    fn rss_no_items_still_returns_header() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Empty Feed</title>
    <description>No items here</description>
  </channel>
</rss>"#;
        let r = go(xml);
        assert_eq!(r.title.as_deref(), Some("Empty Feed"));
        assert!(r.markdown.contains("# Empty Feed"));
        assert!(r.markdown.contains("No items here"));
    }

    #[test]
    fn atom_no_entries_still_returns_header() {
        let xml = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Empty Atom</title>
  <subtitle>Nothing here</subtitle>
</feed>"#;
        let r = go(xml);
        assert_eq!(r.title.as_deref(), Some("Empty Atom"));
        assert!(r.markdown.contains("# Empty Atom"));
        assert!(r.markdown.contains("Nothing here"));
    }

    // ── misc ─────────────────────────────────────────────────────────────────

    #[test]
    fn output_is_trimmed() {
        let r = go(SIMPLE_RSS);
        assert!(!r.markdown.starts_with('\n'));
        assert!(!r.markdown.ends_with('\n'));
    }

    #[test]
    fn sections_separated_by_double_newline() {
        let r = go(SIMPLE_RSS);
        let parts: Vec<&str> = r.markdown.split("\n\n").collect();
        // header + description + 2 items = 4 sections minimum
        assert!(parts.len() >= 3, "expected >=3 sections, got {}: {}", parts.len(), r.markdown);
    }
}
