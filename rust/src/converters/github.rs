// TODO(phase-url): constructed by the registry once Markit::convert_url lands.
#![allow(dead_code)]

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::types::{ConversionResult, Converter, StreamInfo};

const GITHUB_HOSTS: &[&str] = &["github.com", "www.github.com", "gist.github.com"];

pub struct HttpResponse {
    pub status: u16,
    pub ok: bool,
    pub body: String,
}

pub trait HttpFetch: Send + Sync {
    fn get(&self, url: &str, accept: Option<&str>) -> Result<HttpResponse>;
}

pub struct UreqFetcher;

impl HttpFetch for UreqFetcher {
    fn get(&self, url: &str, accept: Option<&str>) -> Result<HttpResponse> {
        let builder = ureq::get(url);
        let builder = if let Some(a) = accept {
            builder.header("Accept", a)
        } else {
            builder
        };
        match builder.call() {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let body = response.body_mut().read_to_string()?;
                Ok(HttpResponse {
                    status,
                    ok: status < 400,
                    body,
                })
            }
            Err(ureq::Error::StatusCode(code)) => Ok(HttpResponse {
                status: code,
                ok: false,
                body: String::new(),
            }),
            Err(e) => Err(anyhow!("HTTP error: {}", e)),
        }
    }
}

pub struct GitHubConverter {
    http: Box<dyn HttpFetch>,
}

impl Default for GitHubConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubConverter {
    pub fn new() -> Self {
        Self {
            http: Box::new(UreqFetcher),
        }
    }

    fn do_convert_url(&self, url: &str) -> Result<ConversionResult> {
        let (hostname, segments) =
            parse_github_url(url).ok_or_else(|| anyhow!("Invalid URL: {}", url))?;

        if hostname == "gist.github.com" {
            return self.fetch_gist(url, &segments);
        }

        if segments.len() < 2 {
            return Err(anyhow!("Unsupported GitHub URL: {}", url));
        }

        let owner = &segments[0];
        let repo = &segments[1];
        let type_str = segments.get(2).map(|s| s.as_str());
        let rest: &[String] = if segments.len() > 3 {
            &segments[3..]
        } else {
            &segments[segments.len()..]
        };

        if type_str == Some("blob") && rest.len() >= 2 {
            let ref_ = &rest[0];
            let file_path = rest[1..].join("/");
            return self.fetch_raw_file(owner, repo, ref_, &file_path);
        }

        if (type_str == Some("issues") || type_str == Some("pull")) && !rest.is_empty() {
            if let Ok(number) = rest[0].parse::<u64>() {
                return self.fetch_issue_or_pr(owner, repo, number);
            }
        }

        if type_str.is_none() {
            return self.fetch_readme(owner, repo);
        }

        Err(anyhow!("Unsupported GitHub URL pattern: {}", url))
    }

    fn fetch_readme(&self, owner: &str, repo: &str) -> Result<ConversionResult> {
        let url = format!("https://raw.githubusercontent.com/{owner}/{repo}/HEAD/README.md");
        let res = self.http.get(&url, None)?;
        if !res.ok {
            return Err(anyhow!("Failed to fetch README: {}", res.status));
        }
        let markdown = res.body.trim().to_string();
        let title = extract_first_heading(&markdown).unwrap_or_else(|| format!("{owner}/{repo}"));
        Ok(ConversionResult {
            markdown,
            title: Some(title),
        })
    }

    fn fetch_raw_file(
        &self,
        owner: &str,
        repo: &str,
        ref_: &str,
        file_path: &str,
    ) -> Result<ConversionResult> {
        let url = format!("https://raw.githubusercontent.com/{owner}/{repo}/{ref_}/{file_path}");
        let res = self.http.get(&url, None)?;
        if !res.ok {
            return Err(anyhow!("Failed to fetch file: {}", res.status));
        }
        let content = res.body.trim().to_string();
        let filename = file_path
            .split('/')
            .next_back()
            .unwrap_or(file_path)
            .to_string();

        if file_path.ends_with(".md") || file_path.ends_with(".mdx") {
            let title = extract_first_heading(&content).unwrap_or_else(|| filename.clone());
            return Ok(ConversionResult {
                markdown: content,
                title: Some(title),
            });
        }

        let ext = if filename.contains('.') {
            filename.rsplit('.').next().unwrap_or("")
        } else {
            ""
        };
        let markdown = format!("# {filename}\n\n```{ext}\n{content}\n```");
        Ok(ConversionResult {
            markdown,
            title: Some(filename),
        })
    }

    fn fetch_gist(&self, original_url: &str, segments: &[String]) -> Result<ConversionResult> {
        if segments.len() < 2 {
            return Err(anyhow!("Unsupported gist URL: {}", original_url));
        }
        let owner = &segments[0];
        let id = &segments[1];
        let url = format!("https://gist.githubusercontent.com/{owner}/{id}/raw");
        let res = self.http.get(&url, None)?;
        if !res.ok {
            return Err(anyhow!("Failed to fetch gist: {}", res.status));
        }
        let content = res.body.trim().to_string();
        let title = format!("gist:{id}");
        Ok(ConversionResult {
            markdown: content,
            title: Some(title),
        })
    }

    fn fetch_issue_or_pr(&self, owner: &str, repo: &str, number: u64) -> Result<ConversionResult> {
        let url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}");
        let res = self
            .http
            .get(&url, Some("application/vnd.github.v3+json"))?;
        if !res.ok {
            return Err(anyhow!("Failed to fetch issue/PR: {}", res.status));
        }

        let data: Value = serde_json::from_str(&res.body)?;

        let title = data["title"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| format!("#{number}"));

        let mut parts: Vec<String> = vec![format!("# {title}")];

        let mut meta: Vec<String> = vec![];
        if let Some(login) = data["user"]["login"].as_str() {
            meta.push(format!("@{login}"));
        }
        if let Some(state) = data["state"].as_str() {
            meta.push(state.to_string());
        }
        if let Some(labels) = data["labels"].as_array() {
            if !labels.is_empty() {
                let names: Vec<&str> = labels.iter().filter_map(|l| l["name"].as_str()).collect();
                meta.push(names.join(", "));
            }
        }
        if !meta.is_empty() {
            parts.push(meta.join(" · "));
        }

        if let Some(body) = data["body"].as_str() {
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }

        Ok(ConversionResult {
            markdown: parts.join("\n\n"),
            title: Some(title),
        })
    }
}

impl Converter for GitHubConverter {
    fn name(&self) -> &'static str {
        "github"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        let Some(url) = &info.url else {
            return false;
        };
        let Some((hostname, _)) = parse_github_url(url) else {
            return false;
        };
        GITHUB_HOSTS.contains(&hostname.as_str())
    }

    fn convert_url(&self, url: &str) -> Option<Result<ConversionResult>> {
        Some(self.do_convert_url(url))
    }

    fn convert(&self, _input: &[u8], info: &StreamInfo) -> Result<ConversionResult> {
        if let Some(url) = &info.url {
            self.do_convert_url(url)
        } else {
            Err(anyhow!("GitHub converter requires a URL"))
        }
    }
}

fn parse_github_url(url: &str) -> Option<(String, Vec<String>)> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;

    let (host_part, path_part) = after_scheme.split_once('/').unwrap_or((after_scheme, ""));

    let hostname = host_part.split(':').next()?.to_string();

    let path_clean = path_part.split('?').next().unwrap_or(path_part);
    let path_clean = path_clean.split('#').next().unwrap_or(path_clean);

    let segments: Vec<String> = path_clean
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    Some((hostname, segments))
}

fn extract_first_heading(markdown: &str) -> Option<String> {
    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            let heading = rest.trim();
            if !heading.is_empty() {
                return Some(heading.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;

    struct MockFetcher {
        responses: HashMap<String, (u16, String)>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl MockFetcher {
        fn new() -> Self {
            Self {
                responses: HashMap::new(),
                calls: Arc::new(Mutex::new(vec![])),
            }
        }

        fn add(&mut self, url: &str, status: u16, body: &str) {
            self.responses
                .insert(url.to_string(), (status, body.to_string()));
        }
    }

    impl HttpFetch for MockFetcher {
        fn get(&self, url: &str, _accept: Option<&str>) -> Result<HttpResponse> {
            self.calls.lock().unwrap().push(url.to_string());
            if let Some((status, body)) = self.responses.get(url) {
                Ok(HttpResponse {
                    status: *status,
                    ok: *status < 400,
                    body: body.clone(),
                })
            } else {
                Err(anyhow!("MockFetcher: no response for: {}", url))
            }
        }
    }

    fn make(mock: MockFetcher) -> (GitHubConverter, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::clone(&mock.calls);
        let c = GitHubConverter {
            http: Box::new(mock),
        };
        (c, calls)
    }

    #[test]
    fn matches_github_com_urls() {
        let c = GitHubConverter::new();
        assert!(c.accepts(&StreamInfo {
            url: Some("https://github.com/owner/repo".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn matches_gist_github_com_urls() {
        let c = GitHubConverter::new();
        assert!(c.accepts(&StreamInfo {
            url: Some("https://gist.github.com/owner/abc123".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn rejects_non_github_urls() {
        let c = GitHubConverter::new();
        assert!(!c.accepts(&StreamInfo {
            url: Some("https://example.com".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn rejects_when_no_url() {
        let c = GitHubConverter::new();
        assert!(!c.accepts(&StreamInfo {
            extension: Some(".md".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn fetches_raw_readme_from_raw_githubusercontent_com() {
        let expected_url = "https://raw.githubusercontent.com/owner/repo/HEAD/README.md";
        let mut mock = MockFetcher::new();
        mock.add(expected_url, 200, "# My Project\n\nSome description.");
        let (c, calls) = make(mock);

        let result = c.do_convert_url("https://github.com/owner/repo").unwrap();

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0], expected_url);
        assert_eq!(result.title.as_deref(), Some("My Project"));
        assert_eq!(result.markdown, "# My Project\n\nSome description.");
    }

    #[test]
    fn fetches_raw_file_and_wraps_in_code_block() {
        let expected_url = "https://raw.githubusercontent.com/owner/repo/main/src/index.ts";
        let mut mock = MockFetcher::new();
        mock.add(expected_url, 200, r#"console.log("hello");"#);
        let (c, calls) = make(mock);

        let result = c
            .do_convert_url("https://github.com/owner/repo/blob/main/src/index.ts")
            .unwrap();

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0], expected_url);
        assert_eq!(result.title.as_deref(), Some("index.ts"));
        assert_eq!(
            result.markdown,
            "# index.ts\n\n```ts\nconsole.log(\"hello\");\n```"
        );
    }

    #[test]
    fn returns_markdown_files_as_is() {
        let url = "https://raw.githubusercontent.com/owner/repo/main/docs/README.md";
        let mut mock = MockFetcher::new();
        mock.add(url, 200, "# Docs\n\nHello world.");
        let (c, _) = make(mock);

        let result = c
            .do_convert_url("https://github.com/owner/repo/blob/main/docs/README.md")
            .unwrap();

        assert_eq!(result.title.as_deref(), Some("Docs"));
        assert_eq!(result.markdown, "# Docs\n\nHello world.");
    }

    #[test]
    fn fetches_raw_gist_content() {
        let expected_url = "https://gist.githubusercontent.com/defunkt/2059/raw";
        let mut mock = MockFetcher::new();
        mock.add(expected_url, 200, "puts 'hello'");
        let (c, calls) = make(mock);

        let result = c
            .do_convert_url("https://gist.github.com/defunkt/2059")
            .unwrap();

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0], expected_url);
        assert_eq!(result.title.as_deref(), Some("gist:2059"));
        assert_eq!(result.markdown, "puts 'hello'");
    }

    #[test]
    fn fetches_issue_from_github_api() {
        let expected_url = "https://api.github.com/repos/owner/repo/issues/42";
        let body = serde_json::json!({
            "title": "Bug report",
            "body": "Something broke.",
            "user": { "login": "alice" },
            "state": "open",
            "labels": [{ "name": "bug" }]
        })
        .to_string();

        let mut mock = MockFetcher::new();
        mock.add(expected_url, 200, &body);
        let (c, calls) = make(mock);

        let result = c
            .do_convert_url("https://github.com/owner/repo/issues/42")
            .unwrap();

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0], expected_url);
        assert_eq!(result.title.as_deref(), Some("Bug report"));
        assert!(result.markdown.contains("# Bug report"));
        assert!(result.markdown.contains("@alice"));
        assert!(result.markdown.contains("open"));
        assert!(result.markdown.contains("bug"));
        assert!(result.markdown.contains("Something broke."));
    }

    #[test]
    fn fetches_pr_via_issues_api_endpoint() {
        let expected_url = "https://api.github.com/repos/owner/repo/issues/7";
        let body = serde_json::json!({
            "title": "Add feature",
            "body": "This PR adds a feature.",
            "user": { "login": "bob" },
            "state": "closed",
            "labels": []
        })
        .to_string();

        let mut mock = MockFetcher::new();
        mock.add(expected_url, 200, &body);
        let (c, calls) = make(mock);

        let result = c
            .do_convert_url("https://github.com/owner/repo/pull/7")
            .unwrap();

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0], expected_url);
        assert_eq!(result.title.as_deref(), Some("Add feature"));
        assert!(result.markdown.contains("@bob"));
    }

    #[test]
    fn throws_on_github_com_with_no_repo() {
        let c = GitHubConverter::new();
        let err = c.do_convert_url("https://github.com/owner").unwrap_err();
        assert!(
            err.to_string().contains("Unsupported GitHub URL"),
            "error was: {err}"
        );
    }

    #[test]
    fn throws_on_unrecognized_subpath() {
        let c = GitHubConverter::new();
        let err = c
            .do_convert_url("https://github.com/owner/repo/wiki")
            .unwrap_err();
        assert!(
            err.to_string().contains("Unsupported GitHub URL pattern"),
            "error was: {err}"
        );
    }

    #[test]
    fn non_github_url_is_not_accepted_by_github_converter() {
        let c = GitHubConverter::new();
        assert!(!c.accepts(&StreamInfo {
            url: Some("https://example.com/article".into()),
            ..Default::default()
        }));
    }
}
