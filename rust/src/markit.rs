use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::converters::audio::AudioConverter;
use crate::converters::csv::CsvConverter;
use crate::converters::docx::DocxConverter;
use crate::converters::epub::EpubConverter;
use crate::converters::github::GitHubConverter;
use crate::converters::html::HtmlConverter;
use crate::converters::image::ImageConverter;
use crate::converters::ipynb::IpynbConverter;
use crate::converters::iwork::IWorkConverter;
use crate::converters::json::JsonConverter;
use crate::converters::pdf::index::PdfConverter;
use crate::converters::plain_text::PlainTextConverter;
use crate::converters::pptx::PptxConverter;
use crate::converters::rss::RssConverter;
use crate::converters::wikipedia::WikipediaConverter;
use crate::converters::xlsx::XlsxConverter;
use crate::converters::xml::XmlConverter;
use crate::converters::yaml::YamlConverter;
use crate::converters::zip::ZipConverter;
use crate::discover_markdown_source::discover_markdown_source;
use crate::types::{ConversionResult, Converter, StreamInfo};

// Mirrors the TS constant in src/markit.ts, which has lagged the package
// version since 0.1.0 — kept identical for request-fingerprint parity.
const USER_AGENT: &str = "markit/0.1.0";

// ── Injectable HTTP trait ────────────────────────────────────────────

/// Minimal HTTP response.
pub struct HttpResponse {
    pub status: u16,
    pub ok: bool,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// Injectable HTTP interface so tests never hit the network.
pub trait HttpFetch: Send + Sync {
    fn request(&self, method: &str, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse>;
}

/// Default ureq-backed fetcher.
pub struct UreqFetcher;

impl HttpFetch for UreqFetcher {
    fn request(&self, method: &str, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        // We only need GET and HEAD for URL conversion.
        // POST has a different type (WithBody) — not needed here.
        let mut builder = if method == "HEAD" {
            ureq::head(url)
        } else {
            ureq::get(url)
        };
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        match builder.call() {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok().map(|s| s.to_string()))
                    .unwrap_or_default();
                let body = response.body_mut().read_to_vec()?;
                Ok(HttpResponse {
                    status,
                    ok: status < 400,
                    content_type,
                    body,
                })
            }
            Err(ureq::Error::StatusCode(code)) => Ok(HttpResponse {
                status: code,
                ok: false,
                content_type: String::new(),
                body: Vec::new(),
            }),
            Err(e) => Err(anyhow!("HTTP error: {}", e)),
        }
    }
}

// ── Markit ────────────────────────────────────────────────────────────

pub struct Markit {
    converters: Vec<Box<dyn Converter>>,
    http: Box<dyn HttpFetch>,
}

/// Built-in converters in registry order: specific formats first, generic
/// last. Mirrors the ordering in src/markit.ts.
pub fn builtin_specific() -> Vec<Box<dyn Converter>> {
    vec![
        Box::new(PdfConverter),
        Box::new(DocxConverter),
        Box::new(PptxConverter),
        Box::new(XlsxConverter),
        Box::new(EpubConverter),
        Box::new(IpynbConverter),
        Box::new(IWorkConverter),
        Box::new(GitHubConverter::new()),
        Box::new(WikipediaConverter),
        Box::new(RssConverter),
        Box::new(CsvConverter),
        Box::new(JsonConverter),
        Box::new(YamlConverter),
        Box::new(ImageConverter),
        Box::new(AudioConverter),
    ]
}

pub fn builtin_generic() -> Vec<Box<dyn Converter>> {
    vec![Box::new(XmlConverter), Box::new(HtmlConverter)]
}

impl Default for Markit {
    fn default() -> Self {
        Self::new()
    }
}

impl Markit {
    pub fn new() -> Self {
        Self::with_http(Box::new(UreqFetcher))
    }

    /// Constructor with injectable HTTP for testing.
    pub fn with_http(http: Box<dyn HttpFetch>) -> Self {
        // ZIP gets its own fresh instances of every non-zip converter for
        // recursive extraction (TS shares the array; boxed trait objects
        // cannot be shared, so we construct twice).
        #[allow(clippy::arc_with_non_send_sync)] // single-threaded; Arc only for shared ownership
        let zip_parent_converters: Arc<Vec<Box<dyn Converter>>> = Arc::new(
            builtin_specific()
                .into_iter()
                .chain(builtin_generic())
                .collect(),
        );
        let zip = ZipConverter::new(Arc::clone(&zip_parent_converters));

        // Registry: specific, zip, generic, plain text last.
        let mut converters: Vec<Box<dyn Converter>> = builtin_specific();
        converters.push(Box::new(zip));
        converters.extend(builtin_generic());
        converters.push(Box::new(PlainTextConverter));

        Self { converters, http }
    }

    /// Returns the builtin converter names in registry order.
    pub fn converter_names(&self) -> Vec<String> {
        self.converters
            .iter()
            .map(|c| c.name().to_string())
            .collect()
    }

    /// Convert a local file to markdown.
    #[allow(dead_code)] // Library-API parity with TS Markit.convertFile(path)
    pub fn convert_file(&self, path: &str) -> Result<ConversionResult> {
        self.convert_file_with(path, StreamInfo::default())
    }

    /// Convert a local file to markdown with extra StreamInfo fields
    /// (TS: convertFile(path, extra) — e.g. imageDir).
    pub fn convert_file_with(&self, path: &str, extra: StreamInfo) -> Result<ConversionResult> {
        let buffer = std::fs::read(path)?;
        let p = Path::new(path);
        let info = StreamInfo {
            local_path: Some(path.to_string()),
            extension: p
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_lowercase())),
            filename: p.file_name().map(|f| f.to_string_lossy().into_owned()),
            ..extra
        };
        self.convert(&buffer, &info)
    }

    /// Convert a URL to markdown.
    pub fn convert_url(&self, url: &str) -> Result<ConversionResult> {
        // Let converters with a URL-specific hook handle it first
        let stream_info = StreamInfo {
            url: Some(url.to_string()),
            ..Default::default()
        };
        for converter in &self.converters {
            if !converter.accepts(&stream_info) {
                continue;
            }
            if let Some(result) = converter.convert_url(url) {
                match result {
                    Ok(r) => return Ok(r),
                    Err(_) => continue, // Fall through to default fetch path
                }
            }
        }

        // For root URLs, check if the site has /llms.txt and return it if so
        if let Some(origin) = extract_origin(url) {
            if let Some(result) = self.try_llms_txt(&origin) {
                return Ok(result);
            }
        }

        // Fetch with content negotiation
        let response = self.http.request(
            "GET",
            url,
            &[
                (
                    "Accept",
                    "text/markdown, text/html;q=0.9, text/plain;q=0.8, */*;q=0.1",
                ),
                ("User-Agent", USER_AGENT),
            ],
        )?;

        if !response.ok {
            return Err(anyhow!("Failed to fetch {}: {}", url, response.status));
        }

        let content_type = &response.content_type;
        let mimetype = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        let url_path = extract_path(url);
        let ext = path_extname(&url_path);

        // Content negotiation worked — server returned markdown directly
        if mimetype == "text/markdown" {
            let filename = path_basename(&url_path);
            return self.convert(
                &response.body,
                &StreamInfo {
                    url: Some(url.to_string()),
                    mimetype: Some("text/markdown".to_string()),
                    extension: Some(".md".to_string()),
                    filename: if filename.is_empty() {
                        None
                    } else {
                        Some(filename)
                    },
                    ..Default::default()
                },
            );
        }

        // For HTML responses, try to discover a raw markdown source.
        if mimetype == "text/html" {
            if let Some(result) = self.try_markdown_source(&response.body, url, &ext) {
                return Ok(result);
            }
        }

        let filename = path_basename(&url_path);
        self.convert(
            &response.body,
            &StreamInfo {
                url: Some(url.to_string()),
                mimetype: if mimetype.is_empty() {
                    None
                } else {
                    Some(mimetype)
                },
                extension: if ext.is_empty() { None } else { Some(ext) },
                filename: if filename.is_empty() {
                    None
                } else {
                    Some(filename)
                },
                ..Default::default()
            },
        )
    }

    /// For root URLs, check if the site publishes /llms.txt.
    fn try_llms_txt(&self, origin: &str) -> Option<ConversionResult> {
        let llms_txt_url = format!("{}/llms.txt", origin);
        let response = self
            .http
            .request("HEAD", &llms_txt_url, &[("User-Agent", USER_AGENT)])
            .ok()?;

        if !response.ok {
            return None;
        }

        let ct = response.content_type.split(';').next().unwrap_or("").trim();
        if !ct.contains("markdown") && !ct.contains("text/plain") && !ct.contains("text/html") {
            return None;
        }

        // HEAD succeeded — now GET the content
        let get_response = self
            .http
            .request("GET", &llms_txt_url, &[("User-Agent", USER_AGENT)])
            .ok()?;

        if !get_response.ok {
            return None;
        }

        let markdown = String::from_utf8_lossy(&get_response.body).to_string();
        Some(ConversionResult::markdown(markdown))
    }

    /// Inspect an HTML response for a discoverable markdown source URL.
    fn try_markdown_source(
        &self,
        html_buffer: &[u8],
        url: &str,
        ext: &str,
    ) -> Option<ConversionResult> {
        let len = html_buffer.len().min(50_000);
        let html = String::from_utf8_lossy(&html_buffer[..len]);

        let md_source_url = discover_markdown_source(&html, url, ext)?;
        self.fetch_markdown_source(&md_source_url)
    }

    /// Fetch a markdown source URL, validating the response is actually markdown.
    fn fetch_markdown_source(&self, md_url: &str) -> Option<ConversionResult> {
        let response = self
            .http
            .request("GET", md_url, &[("User-Agent", USER_AGENT)])
            .ok()?;

        if !response.ok {
            return None;
        }

        let ct = response.content_type.split(';').next().unwrap_or("").trim();
        if !ct.contains("markdown") && !ct.contains("text/plain") {
            return None;
        }

        let filename = path_basename(&extract_path(md_url));
        self.convert(
            &response.body,
            &StreamInfo {
                url: Some(md_url.to_string()),
                mimetype: Some("text/markdown".to_string()),
                extension: Some(".md".to_string()),
                filename: if filename.is_empty() {
                    None
                } else {
                    Some(filename)
                },
                ..Default::default()
            },
        )
        .ok()
    }

    /// Convert a buffer with stream info to markdown.
    pub fn convert(&self, input: &[u8], info: &StreamInfo) -> Result<ConversionResult> {
        let mut errors: Vec<(&'static str, anyhow::Error)> = Vec::new();
        let mut detected_info;
        let info =
            if info.extension.is_none() && info.mimetype.is_none() && input.starts_with(b"%PDF-") {
                detected_info = info.clone();
                detected_info.mimetype = Some("application/pdf".into());
                &detected_info
            } else {
                info
            };

        for converter in &self.converters {
            if !converter.accepts(info) {
                continue;
            }
            match converter.convert(input, info) {
                Ok(result) => return Ok(result),
                Err(err) => errors.push((converter.name(), err)),
            }
        }

        if !errors.is_empty() {
            let details = errors
                .iter()
                .map(|(name, err)| format!("  {name}: {err}"))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(anyhow!("Conversion failed:\n{details}"));
        }

        Err(anyhow!(
            "Unsupported format: {}",
            info.extension
                .as_deref()
                .or(info.mimetype.as_deref())
                .unwrap_or("unknown")
        ))
    }
}

// ── URL helpers ──────────────────────────────────────────────────────

/// Check if the URL is a root URL (pathname is "/" or empty) and return origin.
fn extract_origin(url: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;

    let scheme = if url.starts_with("https://") {
        "https"
    } else {
        "http"
    };

    let (host_and_path, _) = after_scheme.split_once('?').unwrap_or((after_scheme, ""));
    let (host_and_path, _) = host_and_path.split_once('#').unwrap_or((host_and_path, ""));

    let (host, path) = host_and_path.split_once('/').unwrap_or((host_and_path, ""));

    // Only for root URLs
    if !path.is_empty() && path != "/" {
        return None;
    }

    Some(format!("{}://{}", scheme, host))
}

/// Extract the pathname from a URL.
fn extract_path(url: &str) -> String {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    let without_query = after_scheme.split('?').next().unwrap_or(after_scheme);
    let without_fragment = without_query.split('#').next().unwrap_or(without_query);

    if let Some(slash_pos) = without_fragment.find('/') {
        without_fragment[slash_pos..].to_string()
    } else {
        "/".to_string()
    }
}

/// Get extension from a path (e.g., "/foo/bar.html" -> ".html").
fn path_extname(path: &str) -> String {
    let filename = path.rsplit('/').next().unwrap_or(path);
    if let Some(dot_pos) = filename.rfind('.') {
        filename[dot_pos..].to_lowercase()
    } else {
        String::new()
    }
}

/// Get basename from a path (e.g., "/foo/bar.html" -> "bar.html").
fn path_basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // ── Mock HTTP ────────────────────────────────────────────────────

    #[derive(Clone)]
    struct MockResponse {
        status: u16,
        content_type: String,
        body: Vec<u8>,
    }

    struct MockHttp {
        responses: HashMap<String, Vec<MockResponse>>,
        calls: Arc<Mutex<Vec<(String, String)>>>, // (method, url)
    }

    impl MockHttp {
        fn new() -> Self {
            Self {
                responses: HashMap::new(),
                calls: Arc::new(Mutex::new(vec![])),
            }
        }

        fn add(&mut self, method: &str, url: &str, status: u16, ct: &str, body: &str) {
            let key = format!("{} {}", method, url);
            self.responses.entry(key).or_default().push(MockResponse {
                status,
                content_type: ct.to_string(),
                body: body.as_bytes().to_vec(),
            });
        }
    }

    impl HttpFetch for MockHttp {
        fn request(
            &self,
            method: &str,
            url: &str,
            _headers: &[(&str, &str)],
        ) -> Result<HttpResponse> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), url.to_string()));
            let key = format!("{} {}", method, url);
            if let Some(responses) = self.responses.get(&key) {
                // Use first available, or last if only one
                let idx = {
                    let count = self
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|(m, u)| *m == method && *u == url)
                        .count();
                    (count.saturating_sub(1)).min(responses.len() - 1)
                };
                let r = &responses[idx];
                Ok(HttpResponse {
                    status: r.status,
                    ok: r.status < 400,
                    content_type: r.content_type.clone(),
                    body: r.body.clone(),
                })
            } else {
                Err(anyhow!("MockHttp: no response for {} {}", method, url))
            }
        }
    }

    fn make_markit(mock: MockHttp) -> Markit {
        Markit::with_http(Box::new(mock))
    }

    /// Prepend a converter to the registry (test-only) to exercise
    /// convert_url hook dispatch with a mock converter.
    fn prepend_converter(markit: &mut Markit, converter: Box<dyn Converter>) {
        markit.converters.insert(0, converter);
    }

    // ── Test: converter convert_url hooks ────────────────────────────

    /// A mock converter that intercepts URLs matching a prefix via convert_url.
    struct MockUrlConverter {
        url_prefix: String,
        result_markdown: String,
    }

    impl Converter for MockUrlConverter {
        fn name(&self) -> &'static str {
            "mock-url"
        }

        fn accepts(&self, info: &StreamInfo) -> bool {
            info.url
                .as_ref()
                .is_some_and(|u| u.starts_with(&self.url_prefix))
        }

        fn convert(&self, _input: &[u8], _info: &StreamInfo) -> Result<ConversionResult> {
            Ok(ConversionResult::markdown(&self.result_markdown))
        }

        fn convert_url(&self, url: &str) -> Option<Result<ConversionResult>> {
            if url.starts_with(&self.url_prefix) {
                Some(Ok(ConversionResult::markdown(&self.result_markdown)))
            } else {
                None
            }
        }
    }

    #[test]
    fn convert_url_dispatches_to_converter_hook() {
        // Use a mock converter with a convert_url hook.
        // This verifies that Markit::convert_url delegates to converter hooks
        // before falling through to the generic fetch path.
        let mock_converter = MockUrlConverter {
            url_prefix: "https://custom.example.com/".to_string(),
            result_markdown: "# Hook Result

Handled by converter hook."
                .to_string(),
        };

        let mock_http = MockHttp::new();
        // No HTTP mocks needed — the converter hook handles it directly.

        let mut markit = Markit::with_http(Box::new(mock_http));
        prepend_converter(&mut markit, Box::new(mock_converter));
        let result = markit
            .convert_url("https://custom.example.com/page")
            .unwrap();
        assert!(result.markdown.contains("Hook Result"));
        assert!(result.markdown.contains("Handled by converter hook"));
    }

    // ── Test: llms.txt path ──────────────────────────────────────────

    #[test]
    fn convert_url_tries_llms_txt_for_root_urls() {
        let mut mock = MockHttp::new();
        mock.add(
            "HEAD",
            "https://example.com/llms.txt",
            200,
            "text/plain",
            "",
        );
        mock.add(
            "GET",
            "https://example.com/llms.txt",
            200,
            "text/plain",
            "# Example LLM Info\n\nThis site is about examples.",
        );

        let markit = make_markit(mock);
        let result = markit.convert_url("https://example.com/").unwrap();
        assert!(result.markdown.contains("Example LLM Info"));
    }

    #[test]
    fn convert_url_skips_llms_txt_when_head_returns_404() {
        let mut mock = MockHttp::new();
        mock.add("HEAD", "https://example.com/llms.txt", 404, "", "");
        // Falls through to fetching the URL itself
        mock.add(
            "GET",
            "https://example.com/",
            200,
            "text/html",
            "<html><body><h1>Welcome</h1></body></html>",
        );

        let markit = make_markit(mock);
        let result = markit.convert_url("https://example.com/").unwrap();
        // Should have converted the HTML
        assert!(result.markdown.contains("Welcome"));
    }

    #[test]
    fn convert_url_skips_llms_txt_when_content_type_is_wrong() {
        let mut mock = MockHttp::new();
        mock.add(
            "HEAD",
            "https://example.com/llms.txt",
            200,
            "application/json",
            "",
        );
        // Falls through to fetching the URL itself
        mock.add(
            "GET",
            "https://example.com/",
            200,
            "text/html",
            "<html><body><h1>Hello</h1></body></html>",
        );

        let markit = make_markit(mock);
        let result = markit.convert_url("https://example.com/").unwrap();
        assert!(result.markdown.contains("Hello"));
    }

    #[test]
    fn convert_url_does_not_try_llms_txt_for_non_root_urls() {
        let mut mock = MockHttp::new();
        // No HEAD/GET for llms.txt — if it tried, the mock would error
        mock.add(
            "GET",
            "https://example.com/about",
            200,
            "text/html",
            "<html><body><h1>About</h1></body></html>",
        );

        let markit = make_markit(mock);
        let result = markit.convert_url("https://example.com/about").unwrap();
        assert!(result.markdown.contains("About"));
    }

    // ── Test: content negotiation ────────────────────────────────────

    #[test]
    fn convert_url_returns_markdown_directly_when_server_sends_text_markdown() {
        let mut mock = MockHttp::new();
        mock.add(
            "GET",
            "https://example.com/doc",
            200,
            "text/markdown",
            "# Direct Markdown\n\nServed as markdown.",
        );

        let markit = make_markit(mock);
        let result = markit.convert_url("https://example.com/doc").unwrap();
        assert!(result.markdown.contains("Direct Markdown"));
    }

    #[test]
    fn convert_url_errors_on_non_ok_response() {
        let mut mock = MockHttp::new();
        mock.add("GET", "https://example.com/missing", 404, "", "");

        let markit = make_markit(mock);
        let err = markit
            .convert_url("https://example.com/missing")
            .unwrap_err();
        assert!(err.to_string().contains("Failed to fetch"));
        assert!(err.to_string().contains("404"));
    }

    // ── Test: markdown-source discovery path ─────────────────────────

    #[test]
    fn convert_url_discovers_markdown_source_from_link_alternate() {
        let html = r#"<html><head>
            <link rel="alternate" type="text/markdown" href="/post.md">
        </head><body><h1>Blog Post</h1></body></html>"#;

        let mut mock = MockHttp::new();
        mock.add(
            "GET",
            "https://blog.example.com/post",
            200,
            "text/html",
            html,
        );
        mock.add(
            "GET",
            "https://blog.example.com/post.md",
            200,
            "text/markdown",
            "# Blog Post\n\nOriginal markdown content.",
        );

        let markit = make_markit(mock);
        let result = markit.convert_url("https://blog.example.com/post").unwrap();
        assert!(result.markdown.contains("Original markdown content"));
    }

    #[test]
    fn convert_url_discovers_vitepress_markdown_source() {
        let html = r#"<html><head></head><body>
            <div id="VPContent"><main>Rendered docs</main></div>
        </body></html>"#;

        let mut mock = MockHttp::new();
        mock.add(
            "GET",
            "https://docs.example.com/guide/intro",
            200,
            "text/html",
            html,
        );
        mock.add(
            "GET",
            "https://docs.example.com/guide/intro.md",
            200,
            "text/plain",
            "# Introduction\n\nWelcome to the docs.",
        );

        let markit = make_markit(mock);
        let result = markit
            .convert_url("https://docs.example.com/guide/intro")
            .unwrap();
        assert!(result.markdown.contains("Welcome to the docs"));
    }

    #[test]
    fn convert_url_falls_back_to_html_conversion_when_md_source_not_found() {
        let html = r#"<html><body><h1>Regular Page</h1><p>No markdown source.</p></body></html>"#;

        let mut mock = MockHttp::new();
        mock.add("GET", "https://example.com/page", 200, "text/html", html);

        let markit = make_markit(mock);
        let result = markit.convert_url("https://example.com/page").unwrap();
        assert!(result.markdown.contains("Regular Page"));
    }

    #[test]
    fn convert_url_falls_back_when_md_source_returns_wrong_content_type() {
        let html = r#"<html><head>
            <link rel="alternate" type="text/markdown" href="/post.md">
        </head><body><h1>Post</h1></body></html>"#;

        let mut mock = MockHttp::new();
        mock.add("GET", "https://example.com/post", 200, "text/html", html);
        // The .md URL returns HTML (wrong content type)
        mock.add(
            "GET",
            "https://example.com/post.md",
            200,
            "text/html",
            "<html><body>Not markdown</body></html>",
        );

        let markit = make_markit(mock);
        let result = markit.convert_url("https://example.com/post").unwrap();
        // Should fall back to converting the original HTML
        assert!(result.markdown.contains("Post"));
    }

    #[test]
    fn convert_url_falls_back_when_md_source_returns_404() {
        let html = r#"<html><head>
            <link rel="alternate" type="text/markdown" href="/missing.md">
        </head><body><h1>Page</h1></body></html>"#;

        let mut mock = MockHttp::new();
        mock.add("GET", "https://example.com/page", 200, "text/html", html);
        mock.add("GET", "https://example.com/missing.md", 404, "", "");

        let markit = make_markit(mock);
        let result = markit.convert_url("https://example.com/page").unwrap();
        // Falls back to HTML conversion
        assert!(result.markdown.contains("Page"));
    }

    // ── URL helper tests ─────────────────────────────────────────────

    #[test]
    fn extract_origin_root_with_trailing_slash() {
        assert_eq!(
            extract_origin("https://example.com/"),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn extract_origin_root_without_trailing_slash() {
        assert_eq!(
            extract_origin("https://example.com"),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn extract_origin_non_root_returns_none() {
        assert_eq!(extract_origin("https://example.com/about"), None);
    }

    #[test]
    fn path_extname_works() {
        assert_eq!(path_extname("/foo/bar.html"), ".html");
        assert_eq!(path_extname("/foo/bar"), "");
        assert_eq!(path_extname("/foo/bar.PDF"), ".pdf");
    }

    #[test]
    fn path_basename_works() {
        assert_eq!(path_basename("/foo/bar.html"), "bar.html");
        assert_eq!(path_basename("/"), "");
    }

    #[test]
    fn extract_path_works() {
        assert_eq!(extract_path("https://example.com/foo/bar"), "/foo/bar");
        assert_eq!(extract_path("https://example.com/foo?q=1"), "/foo");
        assert_eq!(extract_path("https://example.com"), "/");
    }

    #[test]
    fn converts_pdf_magic_without_stream_metadata() {
        let input = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 42 >> stream
BT /F1 12 Tf 20 100 Td (stdin pdf) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let result = Markit::new()
            .convert(input, &StreamInfo::default())
            .unwrap();
        assert!(result.markdown.contains("stdin pdf"), "{}", result.markdown);
        assert!(!result.markdown.starts_with("%PDF-"));
    }
}
