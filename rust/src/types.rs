use anyhow::Result;

/// Metadata about the stream being converted.
#[derive(Debug, Clone, Default)]
pub struct StreamInfo {
    pub mimetype: Option<String>,
    pub extension: Option<String>,
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

/// Describe an image / transcribe audio: receives raw bytes and mimetype,
/// returns markdown/text.
pub type MediaFn = Box<dyn Fn(&[u8], &str) -> Result<String> + Send + Sync>;

/// Options threaded through every conversion.
#[derive(Default)]
pub struct MarkitOptions {
    /// Describe an image, return markdown.
    pub describe: Option<MediaFn>,
    /// Transcribe audio, return text.
    pub transcribe: Option<MediaFn>,
    /// Extra instructions appended to the image description prompt.
    pub prompt: Option<String>,
}

pub trait Converter {
    /// Human-readable name for error messages.
    fn name(&self) -> &'static str;

    /// Quick check: can this converter handle the given stream?
    fn accepts(&self, info: &StreamInfo) -> bool;

    /// Convert the source to markdown.
    fn convert(
        &self,
        input: &[u8],
        info: &StreamInfo,
        options: &MarkitOptions,
    ) -> Result<ConversionResult>;

    /// Optional URL-first hook. When Some, called before the default fetch
    /// so the converter can handle URL fetching itself.
    fn convert_url(
        &self,
        _url: &str,
        _options: &MarkitOptions,
    ) -> Option<Result<ConversionResult>> {
        None
    }
}
