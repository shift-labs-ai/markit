//! WebAssembly (wasm-bindgen) bindings for the markit engine.
//!
//! The browser build converts the bytes it is handed. It has no network
//! stack and no filesystem, so URL fetching and writing images to a
//! directory stay native: the caller fetches, and images stay placeholder
//! comments. Conversion is synchronous; run it in a Web Worker to keep a
//! page responsive.

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use crate::markit::Markit;
use crate::types::{ConversionResult, StreamInfo};

#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &str = r#"
/** Stream metadata. All fields are optional. */
export interface StreamInfo {
  mimetype?: string;
  extension?: string;
  charset?: string;
  filename?: string;
  /** Where the bytes came from; URL-matched converters such as Wikipedia use it. */
  url?: string;
}

export interface ConversionResult {
  markdown: string;
  title?: string;
}
"#;

// ── JS-facing types ──────────────────────────────────────────────────

/// Stream metadata. No `localPath` or `imageDir`: there is no filesystem.
#[derive(Deserialize, Default)]
#[serde(default)]
struct JsStreamInfo {
    mimetype: Option<String>,
    extension: Option<String>,
    charset: Option<String>,
    filename: Option<String>,
    url: Option<String>,
}

impl From<JsStreamInfo> for StreamInfo {
    fn from(js: JsStreamInfo) -> Self {
        StreamInfo {
            mimetype: js.mimetype,
            extension: js.extension,
            charset: js.charset,
            filename: js.filename,
            url: js.url,
            ..Default::default()
        }
    }
}

#[derive(Serialize)]
struct JsConversionResult {
    markdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

impl From<ConversionResult> for JsConversionResult {
    fn from(r: ConversionResult) -> Self {
        JsConversionResult {
            markdown: r.markdown,
            title: r.title,
        }
    }
}

fn reason(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

// ── API ──────────────────────────────────────────────────────────────

/// Convert bytes to markdown. Throws when no converter accepts them or
/// conversion fails.
#[wasm_bindgen(unchecked_return_type = "ConversionResult")]
pub fn convert(
    input: &[u8],
    #[wasm_bindgen(unchecked_optional_param_type = "StreamInfo")] info: JsValue,
) -> Result<JsValue, JsError> {
    let info: JsStreamInfo = if info.is_undefined() || info.is_null() {
        JsStreamInfo::default()
    } else {
        serde_wasm_bindgen::from_value(info).map_err(reason)?
    };
    let result = Markit::new().convert(input, &info.into()).map_err(reason)?;
    serde_wasm_bindgen::to_value(&JsConversionResult::from(result)).map_err(reason)
}
