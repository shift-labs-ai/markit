//! OpenAI provider — port of src/providers/openai.ts.
//!
//! - Image description via chat completions (base64 data URL).
//! - Audio transcription via the Whisper endpoint (multipart/form-data).

use std::sync::Arc;

use anyhow::{anyhow, Result};
use base64::Engine as _;
use serde_json::{json, Value};

use crate::types::MarkitOptions;

use super::types::{HttpPost, ResolvedConfig, UreqHttpClient};

pub const ENV_KEYS: &[&str] = &["OPENAI_API_KEY", "MARKIT_API_KEY"];
pub const DEFAULT_BASE: &str = "https://api.openai.com/v1";
pub const DEFAULT_MODEL: &str = "gpt-4.1-nano";
pub const DEFAULT_TRANSCRIPTION_MODEL: &str = "gpt-4o-mini-transcribe";

/// Map audio MIME type to file extension for the Whisper API filename.
/// Mirrors the TS `mimeToExt` function.
pub fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "audio/mpeg" => ".mp3",
        "audio/wav" => ".wav",
        "audio/mp4" => ".m4a",
        "video/mp4" => ".mp4",
        "audio/ogg" => ".ogg",
        "audio/flac" => ".flac",
        "audio/aac" => ".aac",
        _ => ".mp3",
    }
}

/// Build a multipart/form-data body from individual fields + a file.
/// The boundary must be chosen by the caller (deterministic in tests).
pub fn build_multipart(
    boundary: &str,
    fields: &[(&str, &str)],
    filename: &str,
    file_mime: &str,
    file_data: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    let crlf = b"\r\n";
    let dash_boundary = format!("--{boundary}");

    for (name, value) in fields {
        body.extend_from_slice(dash_boundary.as_bytes());
        body.extend_from_slice(crlf);
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"").as_bytes(),
        );
        body.extend_from_slice(crlf);
        body.extend_from_slice(crlf);
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(crlf);
    }

    // File field
    body.extend_from_slice(dash_boundary.as_bytes());
    body.extend_from_slice(crlf);
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"")
            .as_bytes(),
    );
    body.extend_from_slice(crlf);
    body.extend_from_slice(format!("Content-Type: {file_mime}").as_bytes());
    body.extend_from_slice(crlf);
    body.extend_from_slice(crlf);
    body.extend_from_slice(file_data);
    body.extend_from_slice(crlf);

    // Closing boundary
    body.extend_from_slice(format!("--{boundary}--").as_bytes());
    body.extend_from_slice(crlf);

    body
}

pub struct OpenAiProvider {
    pub(crate) config: ResolvedConfig,
    pub(crate) prompt: String,
    pub(crate) http: Arc<dyn HttpPost>,
}

impl OpenAiProvider {
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

    /// Build the JSON body for the chat completions endpoint.
    /// Exposed for tests — no I/O.
    pub fn build_describe_body(&self, image: &[u8], mimetype: &str) -> Value {
        let b64 = base64::engine::general_purpose::STANDARD.encode(image);
        let data_url = format!("data:{mimetype};base64,{b64}");
        json!({
            "model": self.config.model,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": self.prompt },
                        {
                            "type": "image_url",
                            "image_url": { "url": data_url }
                        }
                    ]
                }
            ],
            "max_tokens": 1024
        })
    }

    /// Build the multipart body for the transcription endpoint.
    /// `boundary` is passed explicitly so tests can assert on the exact bytes.
    pub fn build_transcribe_multipart(
        &self,
        audio: &[u8],
        mimetype: &str,
        boundary: &str,
    ) -> Vec<u8> {
        let model = self
            .config
            .transcription_model
            .as_deref()
            .unwrap_or(DEFAULT_TRANSCRIPTION_MODEL);
        let ext = mime_to_ext(mimetype);
        let filename = format!("audio{ext}");

        build_multipart(boundary, &[("model", model)], &filename, mimetype, audio)
    }

    /// Describe an image via the OpenAI chat completions API.
    pub fn describe(&self, image: &[u8], mimetype: &str) -> Result<String> {
        let body = self.build_describe_body(image, mimetype);
        let body_str = serde_json::to_string(&body)?;

        let url = format!("{}/chat/completions", self.config.api_base);
        let headers = vec![("Authorization", format!("Bearer {}", self.config.api_key))];

        let res = self.http.post_json(&url, headers, &body_str)?;

        if !res.ok {
            return Err(anyhow!("OpenAI API error {}: {}", res.status, res.body));
        }

        let data: Value = serde_json::from_str(&res.body)
            .map_err(|e| anyhow!("Failed to parse OpenAI response: {e}"))?;

        Ok(data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    /// Transcribe audio via the OpenAI Whisper endpoint.
    pub fn transcribe(&self, audio: &[u8], mimetype: &str) -> Result<String> {
        // Use a random-enough boundary for production; tests use a fixed one.
        let boundary = format!(
            "markit-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        );

        let multipart_body = self.build_transcribe_multipart(audio, mimetype, &boundary);
        let content_type = format!("multipart/form-data; boundary={boundary}");

        let url = format!("{}/audio/transcriptions", self.config.api_base);
        let headers = vec![("Authorization", format!("Bearer {}", self.config.api_key))];

        let res = self
            .http
            .post_multipart(&url, headers, &content_type, multipart_body)?;

        if !res.ok {
            return Err(anyhow!(
                "Transcription API error {}: {}",
                res.status,
                res.body
            ));
        }

        let data: Value = serde_json::from_str(&res.body)
            .map_err(|e| anyhow!("Failed to parse transcription response: {e}"))?;

        Ok(data["text"].as_str().unwrap_or("").to_string())
    }

    /// Build a `MarkitOptions` with both describe and transcribe closures.
    pub fn into_options(self) -> MarkitOptions {
        let p = Arc::new(self);
        let p2 = Arc::clone(&p);
        MarkitOptions {
            describe: Some(Box::new(move |image, mimetype| p.describe(image, mimetype))),
            transcribe: Some(Box::new(move |audio, mimetype| {
                p2.transcribe(audio, mimetype)
            })),
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

    #[derive(Default)]
    struct MockHttpPost {
        json_responses: Mutex<HashMap<String, (u16, String)>>,
        multipart_responses: Mutex<HashMap<String, (u16, String)>>,
        json_calls: Mutex<Vec<(String, String)>>, // (url, body)
        multipart_calls: Mutex<Vec<(String, Vec<u8>)>>, // (url, body)
    }

    impl MockHttpPost {
        fn add_json(&self, url: &str, status: u16, body: &str) {
            self.json_responses
                .lock()
                .unwrap()
                .insert(url.to_string(), (status, body.to_string()));
        }

        fn add_multipart(&self, url: &str, status: u16, body: &str) {
            self.multipart_responses
                .lock()
                .unwrap()
                .insert(url.to_string(), (status, body.to_string()));
        }

        fn last_json_call(&self) -> Option<(String, String)> {
            self.json_calls.lock().unwrap().last().cloned()
        }

        fn last_multipart_call(&self) -> Option<(String, Vec<u8>)> {
            self.multipart_calls.lock().unwrap().last().cloned()
        }
    }

    impl HttpPost for MockHttpPost {
        fn post_json(
            &self,
            url: &str,
            _headers: Vec<(&str, String)>,
            body: &str,
        ) -> Result<HttpPostResponse> {
            self.json_calls
                .lock()
                .unwrap()
                .push((url.to_string(), body.to_string()));
            let responses = self.json_responses.lock().unwrap();
            if let Some((status, resp)) = responses.get(url) {
                Ok(HttpPostResponse {
                    status: *status,
                    ok: *status < 400,
                    body: resp.clone(),
                })
            } else {
                Err(anyhow!("MockHttpPost: no json response for {url}"))
            }
        }

        fn post_multipart(
            &self,
            url: &str,
            _headers: Vec<(&str, String)>,
            _content_type: &str,
            body: Vec<u8>,
        ) -> Result<HttpPostResponse> {
            self.multipart_calls
                .lock()
                .unwrap()
                .push((url.to_string(), body));
            let responses = self.multipart_responses.lock().unwrap();
            if let Some((status, resp)) = responses.get(url) {
                Ok(HttpPostResponse {
                    status: *status,
                    ok: *status < 400,
                    body: resp.clone(),
                })
            } else {
                Err(anyhow!("MockHttpPost: no multipart response for {url}"))
            }
        }
    }

    fn make_config() -> ResolvedConfig {
        ResolvedConfig {
            api_key: "test-openai-key".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4.1-nano".to_string(),
            transcription_model: Some("gpt-4o-mini-transcribe".to_string()),
        }
    }

    fn make_provider(mock: Arc<MockHttpPost>) -> OpenAiProvider {
        OpenAiProvider::with_http(
            make_config(),
            "Describe this image in detail.".to_string(),
            mock as Arc<dyn HttpPost>,
        )
    }

    // ── mime_to_ext ─────────────────────────────────────────────────────────

    #[test]
    fn mime_to_ext_known_types() {
        assert_eq!(mime_to_ext("audio/mpeg"), ".mp3");
        assert_eq!(mime_to_ext("audio/wav"), ".wav");
        assert_eq!(mime_to_ext("audio/mp4"), ".m4a");
        assert_eq!(mime_to_ext("video/mp4"), ".mp4");
        assert_eq!(mime_to_ext("audio/ogg"), ".ogg");
        assert_eq!(mime_to_ext("audio/flac"), ".flac");
        assert_eq!(mime_to_ext("audio/aac"), ".aac");
    }

    #[test]
    fn mime_to_ext_unknown_defaults_to_mp3() {
        assert_eq!(mime_to_ext("audio/unknown"), ".mp3");
        assert_eq!(mime_to_ext(""), ".mp3");
    }

    // ── Describe body shape ─────────────────────────────────────────────────

    #[test]
    fn describe_body_shape() {
        let mock = Arc::new(MockHttpPost::default());
        let provider = make_provider(mock);
        let image = b"fake-png-bytes";
        let body = provider.build_describe_body(image, "image/png");

        assert_eq!(body["model"].as_str(), Some("gpt-4.1-nano"));
        assert_eq!(body["max_tokens"].as_u64(), Some(1024));

        let msgs = body["messages"].as_array().unwrap();
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);

        // Text block first
        assert_eq!(content[0]["type"].as_str(), Some("text"));
        assert_eq!(
            content[0]["text"].as_str(),
            Some("Describe this image in detail.")
        );

        // Image URL block
        assert_eq!(content[1]["type"].as_str(), Some("image_url"));
        let url = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"), "url: {url}");
        // Verify the base64 roundtrips
        let b64_part = url.strip_prefix("data:image/png;base64,").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64_part)
            .unwrap();
        assert_eq!(decoded, image);
    }

    #[test]
    fn describe_body_uses_model_from_config() {
        let config = ResolvedConfig {
            model: "gpt-4o".to_string(),
            ..make_config()
        };
        let provider = OpenAiProvider::with_http(
            config,
            "prompt".to_string(),
            Arc::new(MockHttpPost::default()),
        );
        let body = provider.build_describe_body(b"img", "image/jpeg");
        assert_eq!(body["model"].as_str(), Some("gpt-4o"));
    }

    // ── Transcription multipart body ────────────────────────────────────────

    #[test]
    fn transcribe_multipart_contains_model_field() {
        let mock = Arc::new(MockHttpPost::default());
        let provider = make_provider(mock);
        let boundary = "test-boundary-12345";
        let mp = provider.build_transcribe_multipart(b"audio data", "audio/mpeg", boundary);
        let text = String::from_utf8_lossy(&mp);
        assert!(
            text.contains("name=\"model\""),
            "multipart should include model field"
        );
        assert!(
            text.contains("gpt-4o-mini-transcribe"),
            "should use transcription model"
        );
    }

    #[test]
    fn transcribe_multipart_contains_file_field() {
        let mock = Arc::new(MockHttpPost::default());
        let provider = make_provider(mock);
        let boundary = "test-boundary-xyz";
        let audio = b"mp3 audio bytes here";
        let mp = provider.build_transcribe_multipart(audio, "audio/mpeg", boundary);
        let text = String::from_utf8_lossy(&mp);
        assert!(text.contains("name=\"file\""), "should have file field");
        assert!(
            text.contains("filename=\"audio.mp3\""),
            "should have .mp3 extension"
        );
        assert!(text.contains("Content-Type: audio/mpeg"));
    }

    #[test]
    fn transcribe_multipart_uses_correct_extension_for_wav() {
        let mock = Arc::new(MockHttpPost::default());
        let provider = make_provider(mock);
        let mp = provider.build_transcribe_multipart(b"wav data", "audio/wav", "boundary-abc");
        let text = String::from_utf8_lossy(&mp);
        assert!(text.contains("filename=\"audio.wav\""));
    }

    #[test]
    fn transcribe_multipart_contains_audio_bytes() {
        let mock = Arc::new(MockHttpPost::default());
        let provider = make_provider(mock);
        let audio = b"raw audio content";
        let boundary = "test-bdry";
        let mp = provider.build_transcribe_multipart(audio, "audio/mpeg", boundary);
        // The raw bytes must be present in the body
        assert!(mp.windows(audio.len()).any(|w| w == audio));
    }

    #[test]
    fn transcribe_uses_config_transcription_model() {
        let config = ResolvedConfig {
            transcription_model: Some("whisper-1".to_string()),
            ..make_config()
        };
        let provider = OpenAiProvider::with_http(
            config,
            "prompt".to_string(),
            Arc::new(MockHttpPost::default()),
        );
        let mp = provider.build_transcribe_multipart(b"audio", "audio/mpeg", "b");
        let text = String::from_utf8_lossy(&mp);
        assert!(text.contains("whisper-1"), "should use whisper-1 model");
    }

    #[test]
    fn transcribe_falls_back_to_default_transcription_model_when_none() {
        let config = ResolvedConfig {
            transcription_model: None,
            ..make_config()
        };
        let provider = OpenAiProvider::with_http(
            config,
            "prompt".to_string(),
            Arc::new(MockHttpPost::default()),
        );
        let mp = provider.build_transcribe_multipart(b"audio", "audio/mpeg", "b");
        let text = String::from_utf8_lossy(&mp);
        assert!(
            text.contains("gpt-4o-mini-transcribe"),
            "should fall back to default"
        );
    }

    // ── HTTP call mechanics ─────────────────────────────────────────────────

    #[test]
    fn describe_posts_to_chat_completions() {
        let mock = Arc::new(MockHttpPost::default());
        mock.add_json(
            "https://api.openai.com/v1/chat/completions",
            200,
            &serde_json::json!({
                "choices": [{ "message": { "content": "A landscape." } }]
            })
            .to_string(),
        );
        let provider = make_provider(Arc::clone(&mock));
        let result = provider.describe(b"img", "image/png").unwrap();
        assert_eq!(result, "A landscape.");
        let (url, _) = mock.last_json_call().unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn describe_errors_on_non_2xx() {
        let mock = Arc::new(MockHttpPost::default());
        mock.add_json(
            "https://api.openai.com/v1/chat/completions",
            401,
            "Unauthorized",
        );
        let provider = make_provider(Arc::clone(&mock));
        let err = provider.describe(b"img", "image/png").unwrap_err();
        assert!(
            err.to_string().contains("OpenAI API error 401"),
            "got: {err}"
        );
    }

    #[test]
    fn describe_returns_empty_string_when_no_choices() {
        let mock = Arc::new(MockHttpPost::default());
        mock.add_json(
            "https://api.openai.com/v1/chat/completions",
            200,
            &serde_json::json!({ "choices": [] }).to_string(),
        );
        let provider = make_provider(Arc::clone(&mock));
        let result = provider.describe(b"img", "image/png").unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn transcribe_posts_to_audio_transcriptions() {
        let mock = Arc::new(MockHttpPost::default());
        mock.add_multipart(
            "https://api.openai.com/v1/audio/transcriptions",
            200,
            &serde_json::json!({ "text": "Hello world." }).to_string(),
        );
        let provider = make_provider(Arc::clone(&mock));
        let result = provider.transcribe(b"audio data", "audio/mpeg").unwrap();
        assert_eq!(result, "Hello world.");
        let (url, _) = mock.last_multipart_call().unwrap();
        assert_eq!(url, "https://api.openai.com/v1/audio/transcriptions");
    }

    #[test]
    fn transcribe_errors_on_non_2xx() {
        let mock = Arc::new(MockHttpPost::default());
        mock.add_multipart(
            "https://api.openai.com/v1/audio/transcriptions",
            403,
            "Forbidden",
        );
        let provider = make_provider(Arc::clone(&mock));
        let err = provider.transcribe(b"audio", "audio/wav").unwrap_err();
        assert!(
            err.to_string().contains("Transcription API error 403"),
            "got: {err}"
        );
    }

    // ── into_options ────────────────────────────────────────────────────────

    #[test]
    fn into_options_has_both_describe_and_transcribe() {
        let provider = OpenAiProvider::new(make_config(), "prompt".to_string());
        let opts = provider.into_options();
        assert!(opts.describe.is_some(), "describe should be set");
        assert!(opts.transcribe.is_some(), "transcribe should be set");
    }
}
