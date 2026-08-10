use anyhow::Result;

/// Metadata about the stream being converted.
#[derive(Debug, Clone, Default)]
pub struct StreamInfo {
    pub mimetype: Option<String>,
    pub extension: Option<String>,
    /// Struct-shape parity with TS StreamInfo; the Rust converters decode
    /// lossy UTF-8 (the TS CLI never sets a non-UTF-8 charset either).
    #[allow(dead_code)]
    pub charset: Option<String>,
    pub filename: Option<String>,
    pub local_path: Option<String>,
    pub url: Option<String>,
    /// Directory to write extracted images/diagrams.
    pub image_dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConversionResult {
    pub markdown: String,
    pub title: Option<String>,
}

impl ConversionResult {
    pub fn markdown(markdown: impl Into<String>) -> Self {
        Self {
            markdown: markdown.into(),
            title: None,
        }
    }
}

pub trait Converter {
    /// Human-readable name for error messages.
    fn name(&self) -> &'static str;

    /// Quick check: can this converter handle the given stream?
    fn accepts(&self, info: &StreamInfo) -> bool;

    /// Convert the source to markdown.
    fn convert(&self, input: &[u8], info: &StreamInfo) -> Result<ConversionResult>;

    /// Optional URL-first hook. When Some, called before the default fetch
    /// so the converter can handle URL fetching itself.
    fn convert_url(&self, _url: &str) -> Option<Result<ConversionResult>> {
        None
    }
}
