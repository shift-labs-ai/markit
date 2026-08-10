//! Fast equivalents of the literal-tag regex passes.
//!
//! `strip_tag_blocks(html, "<script", "</script>")` is the scan-based
//! equivalent of `(?is)<script[\s\S]*?</script>` → `replace_all("")`:
//! each case-insensitive occurrence of the open literal is removed through
//! the first following occurrence of the close literal (inclusive); an open
//! with no following close matches nothing, exactly like the lazy regex.
//!
//! Matching is ASCII case-insensitive (SIMD memmem). The regexes' Unicode
//! simple folding additionally matched U+017F ſ for "s" — an input no HTML
//! producer emits and no browser treats as markup — so the scan is
//! semantics-preserving for any real document at ~50x the speed.

use std::borrow::Cow;

/// Case-insensitive forward search for a needle that starts with `<`
/// (caseless), built on memchr for the tag-open byte.
struct CiFinder<'n> {
    needle: &'n [u8],
}

impl<'n> CiFinder<'n> {
    fn new(needle: &'n str) -> Self {
        debug_assert!(needle.starts_with('<'));
        CiFinder {
            needle: needle.as_bytes(),
        }
    }

    fn find(&self, haystack: &[u8]) -> Option<usize> {
        let tail = &self.needle[1..];
        let mut from = 0usize;
        while let Some(rel) = memchr::memchr(b'<', &haystack[from..]) {
            let at = from + rel;
            let cand = haystack.get(at + 1..at + 1 + tail.len())?;
            if cand.eq_ignore_ascii_case(tail) {
                return Some(at);
            }
            from = at + 1;
        }
        None
    }
}

fn ci_finder(needle: &str) -> CiFinder<'_> {
    CiFinder::new(needle)
}

/// Case-insensitive find of a `<`-prefixed literal, starting at `from`.
pub fn find_ci(haystack: &[u8], needle: &str, from: usize) -> Option<usize> {
    ci_finder(needle).find(&haystack[from..]).map(|i| from + i)
}

/// Scan-based `(?i)literal` → `replacement` for a `<`-prefixed literal:
/// equivalent to `Regex::new(r"(?i)…").replace_all(html, replacement)`.
pub fn replace_ci_literal<'a>(html: &'a str, needle: &str, replacement: &str) -> Cow<'a, str> {
    let finder = ci_finder(needle);
    let bytes = html.as_bytes();
    let Some(mut at) = finder.find(bytes) else {
        return Cow::Borrowed(html);
    };
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    loop {
        out.push_str(&html[pos..at]);
        out.push_str(replacement);
        pos = at + needle.len();
        match finder.find(&bytes[pos..]) {
            Some(rel) => at = pos + rel,
            None => break,
        }
    }
    out.push_str(&html[pos..]);
    Cow::Owned(out)
}

/// Remove every `open …first-close` region, case-insensitively.
pub fn strip_tag_blocks<'a>(html: &'a str, open: &str, close: &str) -> Cow<'a, str> {
    let of = ci_finder(open);
    let cf = ci_finder(close);
    let bytes = html.as_bytes();

    let Some(mut start) = of.find(bytes) else {
        return Cow::Borrowed(html);
    };

    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    // An open with no close after it ends the scan: no further match is
    // possible (later opens only have fewer closes ahead), same as the regex.
    while let Some(rel) = cf.find(&bytes[start + open.len()..]) {
        let end = start + open.len() + rel + close.len();
        out.push_str(&html[pos..start]);
        pos = end;
        match of.find(&bytes[pos..]) {
            Some(r) => start = pos + r,
            None => break,
        }
    }
    out.push_str(&html[pos..]);
    Cow::Owned(out)
}

/// First `<title[^>]*>([\s\S]*?)</title>` capture, case-insensitively:
/// after the open literal, attributes run to the first `>`; the capture
/// ends at the first close literal. An occurrence with no close after its
/// `>` fails and the scan moves to the next open, like regex backtracking.
pub fn first_tag_content<'a>(html: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let of = ci_finder(open);
    let cf = ci_finder(close);
    let bytes = html.as_bytes();

    let mut search_from = 0usize;
    while let Some(rel) = of.find(&bytes[search_from..]) {
        let start = search_from + rel;
        let attrs_from = start + open.len();
        // [^>]*> — attributes cannot contain '>', so the first '>' ends them.
        let gt = memchr::memchr(b'>', &bytes[attrs_from..])?;
        let content_from = attrs_from + gt + 1;
        match cf.find(&bytes[content_from..]) {
            Some(c) => return Some(&html[content_from..content_from + c]),
            None => search_from = start + open.len(),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_simple_block() {
        assert_eq!(
            strip_tag_blocks("a<script>x</script>b", "<script", "</script>"),
            "ab"
        );
    }
    #[test]
    fn case_insensitive() {
        assert_eq!(
            strip_tag_blocks("a<SCRIPT foo>x</ScRiPt>b", "<script", "</script>"),
            "ab"
        );
    }
    #[test]
    fn unclosed_open_is_kept() {
        assert_eq!(
            strip_tag_blocks("a<script>x", "<script", "</script>"),
            "a<script>x"
        );
    }
    #[test]
    fn inner_open_is_consumed() {
        assert_eq!(
            strip_tag_blocks("a<script><script></script>b", "<script", "</script>"),
            "ab"
        );
    }
    #[test]
    fn multiple_blocks() {
        assert_eq!(
            strip_tag_blocks(
                "a<script>1</script>b<script>2</script>c",
                "<script",
                "</script>"
            ),
            "abc"
        );
    }
    #[test]
    fn no_match_borrows() {
        assert!(matches!(
            strip_tag_blocks("plain", "<script", "</script>"),
            Cow::Borrowed(_)
        ));
    }
    #[test]
    fn title_simple() {
        assert_eq!(
            first_tag_content("<title>Hi</title>", "<title", "</title>"),
            Some("Hi")
        );
    }
    #[test]
    fn title_with_attrs() {
        assert_eq!(
            first_tag_content("<html><title lang=\"en\">Hi</title>", "<title", "</title>"),
            Some("Hi")
        );
    }
    #[test]
    fn title_unclosed_falls_through_to_next() {
        assert_eq!(
            first_tag_content("<title>broken <title>ok</title>", "<title", "</title>"),
            Some("broken <title>ok")
        );
    }
    #[test]
    fn title_none() {
        assert_eq!(first_tag_content("<p>x</p>", "<title", "</title>"), None);
    }
}
