//! The http(s) URLs markit fetches, split into origin, path and query.
//!
//! A fragment never reaches the server, so it is dropped on parse and
//! never appears in a URL built here.

/// An `http://` or `https://` URL split into the parts URL conversion uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpUrl<'a> {
    /// `scheme://authority`, e.g. `https://example.com:8080`.
    pub origin: &'a str,
    /// From the `/` after the authority up to `?` or `#`; `/` when absent.
    pub path: &'a str,
    /// `?…` up to `#`, or empty.
    pub query: &'a str,
}

impl<'a> HttpUrl<'a> {
    pub fn parse(url: &'a str) -> Option<Self> {
        let scheme_len = if url.starts_with("https://") {
            "https://".len()
        } else if url.starts_with("http://") {
            "http://".len()
        } else {
            return None;
        };
        let rest = &url[scheme_len..];
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let origin = &url[..scheme_len + authority_end];
        let tail = &rest[authority_end..];
        let tail = &tail[..tail.find('#').unwrap_or(tail.len())];
        let (path, query) = tail.split_at(tail.find('?').unwrap_or(tail.len()));
        Some(Self {
            origin,
            path: if path.is_empty() { "/" } else { path },
            query,
        })
    }

    /// `http` or `https`.
    pub fn scheme(&self) -> &'a str {
        &self.origin[..self.origin.find(':').unwrap_or(0)]
    }

    /// The authority without its port, e.g. `example.com`.
    pub fn host(&self) -> &'a str {
        let authority = &self.origin[self.scheme().len() + "://".len()..];
        &authority[..authority.find(':').unwrap_or(authority.len())]
    }

    /// Resolve `href` against this URL as a browser resolves a link:
    /// absolute and scheme-relative hrefs keep their own host, a rooted
    /// path replaces the path, a relative one joins this path's directory,
    /// and `.`/`..` segments collapse. `None` for another scheme
    /// (`mailto:`, `data:`) or an href that names only a fragment.
    pub fn resolve(&self, href: &str) -> Option<String> {
        let href = &href[..href.find('#').unwrap_or(href.len())];
        if href.is_empty() {
            return None;
        }
        if let Some(rest) = href.strip_prefix("//") {
            let url = format!("{}://{rest}", self.scheme());
            return HttpUrl::parse(&url).map(|u| u.to_string());
        }
        if has_scheme(href) {
            return HttpUrl::parse(href).map(|u| u.to_string());
        }
        let (href_path, href_query) = href.split_at(href.find('?').unwrap_or(href.len()));
        let path = if href_path.is_empty() {
            self.path.to_string()
        } else if href_path.starts_with('/') {
            remove_dot_segments(href_path)
        } else {
            let dir = &self.path[..=self.path.rfind('/').unwrap_or(0)];
            remove_dot_segments(&format!("{dir}{href_path}"))
        };
        Some(format!("{}{}{}", self.origin, path, href_query))
    }
}

impl std::fmt::Display for HttpUrl<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}{}", self.origin, self.path, self.query)
    }
}

/// `scheme:` per RFC 3986: a letter, then letters, digits, `+`, `-`, `.`.
fn has_scheme(href: &str) -> bool {
    let Some(colon) = href.find(':') else {
        return false;
    };
    let scheme = &href[..colon];
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Collapse `.` and `..` segments of a rooted path (RFC 3986 §5.2.4).
/// A path ending in a dot segment keeps its trailing slash.
fn remove_dot_segments(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let mut out: Vec<&str> = Vec::with_capacity(segments.len());
    for (i, segment) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *segment {
            "." => {}
            ".." => {
                out.pop();
            }
            s => {
                out.push(s);
                continue;
            }
        }
        if last {
            out.push("");
        }
    }
    format!("/{}", out.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(url: &str) -> (&str, &str, &str) {
        let u = HttpUrl::parse(url).unwrap();
        (u.origin, u.path, u.query)
    }

    #[test]
    fn parse_splits_origin_path_and_query_and_drops_the_fragment() {
        assert_eq!(
            parts("https://example.com:8080/a/b?x=1&y=/z#frag"),
            ("https://example.com:8080", "/a/b", "?x=1&y=/z")
        );
        assert_eq!(
            parts("https://example.com"),
            ("https://example.com", "/", "")
        );
        assert_eq!(
            parts("http://example.com?q"),
            ("http://example.com", "/", "?q")
        );
        assert_eq!(
            parts("http://example.com#f"),
            ("http://example.com", "/", "")
        );
        assert_eq!(
            parts("https://example.com/a#f?not-query"),
            ("https://example.com", "/a", "")
        );
        assert_eq!(HttpUrl::parse("ftp://example.com/a"), None);
        assert_eq!(HttpUrl::parse("/a/b"), None);
    }

    #[test]
    fn scheme_and_host_come_from_the_origin() {
        let u = HttpUrl::parse("https://example.com:8080/a").unwrap();
        assert_eq!((u.scheme(), u.host()), ("https", "example.com"));
        let u = HttpUrl::parse("http://github.com?tab=/x").unwrap();
        assert_eq!((u.scheme(), u.host()), ("http", "github.com"));
    }

    #[test]
    fn display_rebuilds_the_url_without_its_fragment() {
        let u = HttpUrl::parse("https://example.com/a?b#c").unwrap();
        assert_eq!(u.to_string(), "https://example.com/a?b");
    }

    fn resolve(base: &str, href: &str) -> Option<String> {
        HttpUrl::parse(base).unwrap().resolve(href)
    }

    #[test]
    fn resolve_follows_browser_link_resolution() {
        let base = "https://example.com/docs/guide/page?ref=nav#top";
        let cases = [
            ("https://other.com/x.md", "https://other.com/x.md"),
            ("http://other.com", "http://other.com/"),
            ("//cdn.example.com/x.md", "https://cdn.example.com/x.md"),
            ("/root.md", "https://example.com/root.md"),
            ("page.md", "https://example.com/docs/guide/page.md"),
            ("./page.md", "https://example.com/docs/guide/page.md"),
            ("../page.md", "https://example.com/docs/page.md"),
            ("../../../../page.md", "https://example.com/page.md"),
            (
                "sub/./x/../page.md",
                "https://example.com/docs/guide/sub/page.md",
            ),
            ("..", "https://example.com/docs/"),
            ("page.md?v=2", "https://example.com/docs/guide/page.md?v=2"),
            ("page.md#section", "https://example.com/docs/guide/page.md"),
            ("?v=2", "https://example.com/docs/guide/page?v=2"),
        ];
        for (href, want) in cases {
            assert_eq!(resolve(base, href).as_deref(), Some(want), "href {href}");
        }
    }

    #[test]
    fn resolve_ignores_the_base_query_when_finding_the_directory() {
        assert_eq!(
            resolve("https://example.com/docs/page?next=/a/b", "x.md").as_deref(),
            Some("https://example.com/docs/x.md")
        );
    }

    #[test]
    fn resolve_rejects_other_schemes_and_bare_fragments() {
        for href in [
            "mailto:a@example.com",
            "data:text/markdown,hi",
            "ftp://x/y",
            "#top",
            "",
        ] {
            assert_eq!(
                resolve("https://example.com/page", href),
                None,
                "href {href}"
            );
        }
    }
}
