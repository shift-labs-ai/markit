//! Node-API (napi-rs v2) bindings for the markit engine.
//!
//! Every conversion method returns an `AsyncTask` so the blocking work
//! runs on the libuv thread pool, never on the JS main thread.

use napi::bindgen_prelude::*;
use napi::Task;
use napi_derive::napi;

use crate::markit::Markit as CoreMarkit;
use crate::types::{ConversionResult, Converter, StreamInfo};

// ── JS-facing types ──────────────────────────────────────────────────

/// Stream metadata. All fields are optional strings.
#[napi(object)]
#[derive(Default, Clone)]
pub struct JsStreamInfo {
    pub mimetype: Option<String>,
    pub extension: Option<String>,
    pub charset: Option<String>,
    pub filename: Option<String>,
    pub local_path: Option<String>,
    pub url: Option<String>,
    pub image_dir: Option<String>,
}

impl From<JsStreamInfo> for StreamInfo {
    fn from(js: JsStreamInfo) -> Self {
        StreamInfo {
            mimetype: js.mimetype,
            extension: js.extension,
            charset: js.charset,
            filename: js.filename,
            local_path: js.local_path,
            url: js.url,
            image_dir: js.image_dir,
        }
    }
}

impl From<&StreamInfo> for JsStreamInfo {
    fn from(info: &StreamInfo) -> Self {
        JsStreamInfo {
            mimetype: info.mimetype.clone(),
            extension: info.extension.clone(),
            charset: info.charset.clone(),
            filename: info.filename.clone(),
            local_path: info.local_path.clone(),
            url: info.url.clone(),
            image_dir: info.image_dir.clone(),
        }
    }
}

#[napi(object)]
pub struct JsConversionResult {
    pub markdown: String,
    pub title: Option<String>,
}

impl From<ConversionResult> for JsConversionResult {
    fn from(r: ConversionResult) -> Self {
        JsConversionResult {
            markdown: r.markdown,
            title: r.title,
        }
    }
}

fn reason(e: anyhow::Error) -> Error {
    Error::from_reason(e.to_string())
}

// ── AsyncTask implementations ────────────────────────────────────────

/// Convert a buffer with optional stream info.
pub struct ConvertBufferTask {
    input: Vec<u8>,
    info: StreamInfo,
}

impl Task for ConvertBufferTask {
    type Output = ConversionResult;
    type JsValue = JsConversionResult;

    fn compute(&mut self) -> Result<Self::Output> {
        let m = CoreMarkit::new();
        m.convert(&self.input, &self.info).map_err(reason)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

/// Convert a local file.
pub struct ConvertFileTask {
    path: String,
    extra: StreamInfo,
}

impl Task for ConvertFileTask {
    type Output = ConversionResult;
    type JsValue = JsConversionResult;

    fn compute(&mut self) -> Result<Self::Output> {
        let m = CoreMarkit::new();
        m.convert_file_with(&self.path, self.extra.clone())
            .map_err(reason)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

/// Convert a URL.
pub struct ConvertUrlTask {
    url: String,
}

impl Task for ConvertUrlTask {
    type Output = ConversionResult;
    type JsValue = JsConversionResult;

    fn compute(&mut self) -> Result<Self::Output> {
        let m = CoreMarkit::new();
        m.convert_url(&self.url).map_err(reason)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

/// Convert by named converter — buffer path.
pub struct ConverterConvertTask {
    name: String,
    input: Vec<u8>,
    info: StreamInfo,
}

impl Task for ConverterConvertTask {
    type Output = ConversionResult;
    type JsValue = JsConversionResult;

    fn compute(&mut self) -> Result<Self::Output> {
        let converter = converter_by_name(&self.name)?;
        converter.convert(&self.input, &self.info).map_err(reason)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

/// Convert by named converter — URL path.
pub struct ConverterConvertUrlTask {
    name: String,
    url: String,
}

/// Result wrapper: None means no convert_url hook, Some means hook returned.
pub struct ConverterUrlResult(Option<ConversionResult>);

impl Task for ConverterConvertUrlTask {
    type Output = ConverterUrlResult;
    type JsValue = Option<JsConversionResult>;

    fn compute(&mut self) -> Result<Self::Output> {
        let converter = converter_by_name(&self.name)?;
        match converter.convert_url(&self.url) {
            None => Ok(ConverterUrlResult(None)),
            Some(Ok(r)) => Ok(ConverterUrlResult(Some(r))),
            Some(Err(e)) => Err(reason(e)),
        }
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.0.map(Into::into))
    }
}

// ── Markit class ─────────────────────────────────────────────────────

#[napi(js_name = "Markit")]
pub struct JsMarkit;

#[napi]
impl JsMarkit {
    #[napi(constructor)]
    pub fn new() -> Self {
        JsMarkit
    }

    /// Convert a buffer to markdown.
    #[napi]
    pub fn convert(
        &self,
        input: Buffer,
        info: Option<JsStreamInfo>,
    ) -> AsyncTask<ConvertBufferTask> {
        AsyncTask::new(ConvertBufferTask {
            input: input.to_vec(),
            info: info.unwrap_or_default().into(),
        })
    }

    /// Convert a local file to markdown.
    #[napi]
    pub fn convert_file(
        &self,
        path: String,
        extra: Option<JsStreamInfo>,
    ) -> AsyncTask<ConvertFileTask> {
        AsyncTask::new(ConvertFileTask {
            path,
            extra: extra.unwrap_or_default().into(),
        })
    }

    /// Convert a URL to markdown.
    #[napi]
    pub fn convert_url(&self, url: String) -> AsyncTask<ConvertUrlTask> {
        AsyncTask::new(ConvertUrlTask { url })
    }
}

// ── Standalone functions ─────────────────────────────────────────────

/// Returns the builtin converter names in registry order.
#[napi]
pub fn converter_names() -> Vec<String> {
    let m = CoreMarkit::new();
    m.converter_names()
}

/// Check whether a named converter accepts the given stream info.
#[napi]
pub fn converter_accepts(name: String, info: JsStreamInfo) -> Result<bool> {
    let converter = converter_by_name(&name)?;
    let si: StreamInfo = info.into();
    Ok(converter.accepts(&si))
}

/// Convert using a specific named converter.
#[napi]
pub fn converter_convert(
    name: String,
    input: Buffer,
    info: JsStreamInfo,
) -> AsyncTask<ConverterConvertTask> {
    AsyncTask::new(ConverterConvertTask {
        name,
        input: input.to_vec(),
        info: info.into(),
    })
}

/// Convert a URL using a specific named converter's URL hook.
/// Resolves to null if the converter has no convert_url hook.
#[napi]
pub fn converter_convert_url(name: String, url: String) -> AsyncTask<ConverterConvertUrlTask> {
    AsyncTask::new(ConverterConvertUrlTask { name, url })
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Build a single converter by name. The names match `Converter::name()`
/// for each builtin converter.
fn converter_by_name(name: &str) -> Result<Box<dyn Converter>> {
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
    use std::sync::Arc;

    match name {
        "pdf" => Ok(Box::new(PdfConverter)),
        "docx" => Ok(Box::new(DocxConverter)),
        "pptx" => Ok(Box::new(PptxConverter)),
        "xlsx" => Ok(Box::new(XlsxConverter)),
        "epub" => Ok(Box::new(EpubConverter)),
        "ipynb" => Ok(Box::new(IpynbConverter)),
        "iwork" => Ok(Box::new(IWorkConverter)),
        "github" => Ok(Box::new(GitHubConverter::new())),
        "wikipedia" => Ok(Box::new(WikipediaConverter)),
        "rss" => Ok(Box::new(RssConverter)),
        "csv" => Ok(Box::new(CsvConverter)),
        "json" => Ok(Box::new(JsonConverter)),
        "yaml" => Ok(Box::new(YamlConverter)),
        "image" => Ok(Box::new(ImageConverter)),
        "audio" => Ok(Box::new(AudioConverter)),
        "zip" => {
            use crate::markit::{builtin_generic, builtin_specific};
            // single-threaded; Arc only for shared ownership
            #[allow(clippy::arc_with_non_send_sync)]
            let parent: Arc<Vec<Box<dyn Converter>>> = Arc::new(
                builtin_specific()
                    .into_iter()
                    .chain(builtin_generic())
                    .collect(),
            );
            Ok(Box::new(ZipConverter::new(parent)))
        }
        "xml" => Ok(Box::new(XmlConverter)),
        "html" => Ok(Box::new(HtmlConverter)),
        "plain-text" => Ok(Box::new(PlainTextConverter)),
        _ => Err(Error::from_reason(format!("Unknown converter: {}", name))),
    }
}
