//! Anthropic provider — port of src/providers/anthropic.ts.
//!
//! Describes images via the Anthropic Messages API (base64 payload).
//! Anthropic has no transcription API, so `transcribe` is always None.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use base64::Engine as _;
use serde_json::{json, Value};

use crate::types::MarkitOptions;

use super::types::{HttpPost, ResolvedConfig, UreqHttpClient};

pub const ENV_KEYS: &[&str] = &["ANTHROPIC_API_KEY", "MARKIT_API_KEY"];
pub const DEFAULT_BASE: &str = "https://api.anthropic.com";
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";

pub struct AnthropicProvider {
    pub(crate) config: ResolvedConfig,
    pub(crate) prompt: String,
    pub(crate) http: Arc<dyn HttpPost>,
}

impl AnthropicProvider {
    /// Create with the production ureq HTTP client.
    pub fn new(config: ResolvedConfig, prompt: String) -> Self {
        Self {
            config,
            prompt,
            http: Arc::new(UreqHttpClient),
        }
    }

    /// Create with an injectable HTTP client (for tests).
    #[cfg(test)]
    pub fn with_http(config: ResolvedConfig, prompt: String, http: Arc<dyn HttpPost>) -> Self {
        Self {
            config,
            prompt,
            http,
        }
    }

    /// Build the JSON request body for the Messages API.
    /// Exposed for tests — does not perform any I/O.
    pub fn build_describe_body(&self, image: &[u8], mimetype: &str) -> Value {
        json!({
            "model": self.config.model,
            "max_tokens": 1024,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": mimetype,
                                "data": base64::engine::general_purpose::STANDARD.encode(image),
                            }
                        },
                        { "type": "text", "text": self.prompt }
                    ]
                }
            ]
        })
    }

    /// Describe an image via the Anthropic Messages API.
    pub fn describe(&self, image: &[u8], mimetype: &str) -> Result<String> {
        let body = self.build_describe_body(image, mimetype);
        let body_str = serde_json::to_string(&body)?;

        let url = format!("{}/v1/messages", self.config.api_base);
        let headers = vec![
            ("x-api-key", self.config.api_key.clone()),
            ("anthropic-version", "2023-06-01".to_string()),
        ];

        let res = self.http.post_json(&url, headers, &body_str)?;

        if !res.ok {
            return Err(anyhow!("Anthropic API error {}: {}", res.status, res.body));
        }

        let data: Value = serde_json::from_str(&res.body)
            .map_err(|e| anyhow!("Failed to parse Anthropic response: {e}"))?;

        Ok(data["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    /// Build a `MarkitOptions` with this provider's describe closure.
    /// Called by the provider registry.
    pub fn into_options(self) -> MarkitOptions {
        let p = Arc::new(self);
        MarkitOptions {
            describe: Some(Box::new(move |image, mimetype| p.describe(image, mimetype))),
            transcribe: None, // Anthropic has no transcription API
            prompt: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::super::types::HttpPostResponse;
    use super::*;

    // ── Mock HTTP client ────────────────────────────────────────────────────

    struct MockHttpPost {
        responses: HashMap<String, (u16, String)>,
        calls: Mutex<Vec<(String, String)>>, // (url, body)
    }

    impl MockHttpPost {
        fn new() -> Self {
            Self {
                responses: HashMap::new(),
                calls: Mutex::new(vec![]),
            }
        }

        fn add(&mut self, url: &str, status: u16, body: &str) {
            self.responses
                .insert(url.to_string(), (status, body.to_string()));
        }

        fn last_call(&self) -> Option<(String, String)> {
            self.calls.lock().unwrap().last().cloned()
        }
    }

    impl HttpPost for MockHttpPost {
        fn post_json(
            &self,
            url: &str,
            _headers: Vec<(&str, String)>,
            body: &str,
        ) -> Result<HttpPostResponse> {
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), body.to_string()));
            if let Some((status, resp_body)) = self.responses.get(url) {
                Ok(HttpPostResponse {
                    status: *status,
                    ok: *status < 400,
                    body: resp_body.clone(),
                })
            } else {
                Err(anyhow!("MockHttpPost: no response for {url}"))
            }
        }

        fn post_multipart(
            &self,
            _url: &str,
            _headers: Vec<(&str, String)>,
            _content_type: &str,
            _body: Vec<u8>,
        ) -> Result<HttpPostResponse> {
            Err(anyhow!("MockHttpPost: post_multipart not expected"))
        }
    }

    fn make_config() -> ResolvedConfig {
        ResolvedConfig {
            api_key: "test-key".to_string(),
            api_base: "https://api.anthropic.com".to_string(),
            model: "claude-haiku-4-5".to_string(),
            transcription_model: None,
        }
    }

    fn make_provider_with_mock(mock: MockHttpPost) -> (AnthropicProvider, Arc<MockHttpPost>) {
        let shared = Arc::new(mock);
        let provider = AnthropicProvider::with_http(
            make_config(),
            "Describe this image in detail.".to_string(),
            Arc::clone(&shared) as Arc<dyn HttpPost>,
        );
        (provider, shared)
    }

    // ── Request-body shape ──────────────────────────────────────────────────

    #[test]
    fn describe_body_shape() {
        let mock = MockHttpPost::new();
        let (provider, _) = make_provider_with_mock(mock);
        let image = b"fake-image-bytes";
        let body = provider.build_describe_body(image, "image/png");

        assert_eq!(body["model"].as_str(), Some("claude-haiku-4-5"));
        assert_eq!(body["max_tokens"].as_u64(), Some(1024));

        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"].as_str(), Some("user"));

        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);

        // Image block
        let img_block = &content[0];
        assert_eq!(img_block["type"].as_str(), Some("image"));
        assert_eq!(img_block["source"]["type"].as_str(), Some("base64"));
        assert_eq!(
            img_block["source"]["media_type"].as_str(),
            Some("image/png")
        );
        let data = img_block["source"]["data"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert_eq!(decoded, image);

        // Text block
        let text_block = &content[1];
        assert_eq!(text_block["type"].as_str(), Some("text"));
        assert_eq!(
            text_block["text"].as_str(),
            Some("Describe this image in detail.")
        );
    }

    #[test]
    fn describe_body_uses_custom_prompt() {
        let config = make_config();
        let provider = AnthropicProvider::with_http(
            config,
            "Describe this image in detail.\n\nFocus on the chart data.".to_string(),
            Arc::new(MockHttpPost::new()),
        );
        let body = provider.build_describe_body(b"img", "image/jpeg");
        let text = body["messages"][0]["content"][1]["text"].as_str().unwrap();
        assert!(text.contains("Focus on the chart data."));
    }

    #[test]
    fn describe_body_uses_model_from_config() {
        let config = ResolvedConfig {
            model: "claude-opus-4-5".to_string(),
            ..make_config()
        };
        let provider = AnthropicProvider::with_http(
            config,
            "Describe this image in detail.".to_string(),
            Arc::new(MockHttpPost::new()),
        );
        let body = provider.build_describe_body(b"img", "image/png");
        assert_eq!(body["model"].as_str(), Some("claude-opus-4-5"));
    }

    // ── HTTP call mechanics ─────────────────────────────────────────────────

    #[test]
    fn describe_posts_to_correct_url() {
        let api_response = serde_json::json!({
            "content": [{ "type": "text", "text": "A cat." }]
        })
        .to_string();
        let mut mock = MockHttpPost::new();
        mock.add("https://api.anthropic.com/v1/messages", 200, &api_response);
        let (provider, shared) = make_provider_with_mock(mock);

        let result = provider.describe(b"image data", "image/png").unwrap();
        assert_eq!(result, "A cat.");

        let (url, _) = shared.last_call().unwrap();
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn describe_uses_custom_api_base() {
        let config = ResolvedConfig {
            api_base: "https://proxy.example.com".to_string(),
            ..make_config()
        };
        let api_response = serde_json::json!({
            "content": [{ "type": "text", "text": "A dog." }]
        })
        .to_string();
        let mut mock = MockHttpPost::new();
        mock.add("https://proxy.example.com/v1/messages", 200, &api_response);
        let shared = Arc::new(mock);
        let provider = AnthropicProvider::with_http(
            config,
            "Describe this image in detail.".to_string(),
            Arc::clone(&shared) as Arc<dyn HttpPost>,
        );

        let result = provider.describe(b"image", "image/png").unwrap();
        assert_eq!(result, "A dog.");
    }

    #[test]
    fn describe_returns_first_content_text() {
        let api_response = serde_json::json!({
            "content": [
                { "type": "text", "text": "First block." },
                { "type": "text", "text": "Second block." }
            ]
        })
        .to_string();
        let mut mock = MockHttpPost::new();
        mock.add("https://api.anthropic.com/v1/messages", 200, &api_response);
        let (provider, _) = make_provider_with_mock(mock);

        let result = provider.describe(b"img", "image/png").unwrap();
        assert_eq!(result, "First block.");
    }

    #[test]
    fn describe_returns_empty_string_when_no_content() {
        let api_response = serde_json::json!({ "content": [] }).to_string();
        let mut mock = MockHttpPost::new();
        mock.add("https://api.anthropic.com/v1/messages", 200, &api_response);
        let (provider, _) = make_provider_with_mock(mock);

        let result = provider.describe(b"img", "image/png").unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn describe_errors_on_non_2xx() {
        let mut mock = MockHttpPost::new();
        mock.add("https://api.anthropic.com/v1/messages", 401, "Unauthorized");
        let (provider, _) = make_provider_with_mock(mock);

        let err = provider.describe(b"img", "image/png").unwrap_err();
        assert!(
            err.to_string().contains("Anthropic API error 401"),
            "got: {err}"
        );
    }

    // ── into_options ────────────────────────────────────────────────────────

    #[test]
    fn into_options_has_describe_but_no_transcribe() {
        let provider = AnthropicProvider::new(make_config(), "prompt".to_string());
        let opts = provider.into_options();
        assert!(opts.describe.is_some(), "describe should be set");
        assert!(
            opts.transcribe.is_none(),
            "Anthropic has no transcription API"
        );
    }
}
