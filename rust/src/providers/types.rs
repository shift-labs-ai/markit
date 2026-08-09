//! Shared types for LLM provider implementations.
//! Port of src/providers/types.ts.

use anyhow::Result;

/// Resolved provider configuration after applying env/config precedence.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub api_key: String,
    pub api_base: String,
    pub model: String,
    pub transcription_model: Option<String>,
}

/// Response from an HTTP POST call.
pub struct HttpPostResponse {
    pub status: u16,
    pub ok: bool,
    pub body: String,
}

/// Injectable HTTP POST client — production impl uses ureq; tests use a mock.
pub trait HttpPost: Send + Sync {
    /// POST with a JSON body (Content-Type: application/json).
    fn post_json(
        &self,
        url: &str,
        headers: Vec<(&str, String)>,
        body: &str,
    ) -> Result<HttpPostResponse>;

    /// POST with a pre-built multipart/form-data body.
    fn post_multipart(
        &self,
        url: &str,
        headers: Vec<(&str, String)>,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<HttpPostResponse>;
}

/// Production HTTP client backed by ureq.
pub struct UreqHttpClient;

impl HttpPost for UreqHttpClient {
    fn post_json(
        &self,
        url: &str,
        headers: Vec<(&str, String)>,
        body: &str,
    ) -> Result<HttpPostResponse> {
        let mut builder = ureq::post(url).header("Content-Type", "application/json");
        for (k, v) in &headers {
            builder = builder.header(*k, v.as_str());
        }
        match builder.send(body.as_bytes()) {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let text = response.body_mut().read_to_string()?;
                Ok(HttpPostResponse { status, ok: true, body: text })
            }
            Err(ureq::Error::StatusCode(code)) => Ok(HttpPostResponse {
                status: code,
                ok: false,
                body: String::new(),
            }),
            Err(e) => Err(anyhow::anyhow!("HTTP error: {}", e)),
        }
    }

    fn post_multipart(
        &self,
        url: &str,
        headers: Vec<(&str, String)>,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<HttpPostResponse> {
        let mut builder = ureq::post(url).header("Content-Type", content_type);
        for (k, v) in &headers {
            builder = builder.header(*k, v.as_str());
        }
        match builder.send(body.as_slice()) {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let text = response.body_mut().read_to_string()?;
                Ok(HttpPostResponse { status, ok: true, body: text })
            }
            Err(ureq::Error::StatusCode(code)) => Ok(HttpPostResponse {
                status: code,
                ok: false,
                body: String::new(),
            }),
            Err(e) => Err(anyhow::anyhow!("HTTP error: {}", e)),
        }
    }
}
