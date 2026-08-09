use anyhow::Result;

use super::decode_text;
use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

const MIMETYPES: &[&str] = &["text/xml", "application/xml"];

pub struct XmlConverter;

impl Converter for XmlConverter {
    fn name(&self) -> &'static str {
        "xml"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        matches!(info.extension.as_deref(), Some(".xml") | Some(".svg"))
            || info
                .mimetype
                .as_deref()
                .is_some_and(|m| MIMETYPES.iter().any(|p| m.starts_with(p)))
    }

    fn convert(
        &self,
        input: &[u8],
        info: &StreamInfo,
        _options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let text = decode_text(input);
        let ext = info
            .extension
            .as_deref()
            .map(|e| e.trim_start_matches('.'))
            .filter(|e| !e.is_empty())
            .unwrap_or("xml");
        Ok(ConversionResult::markdown(format!(
            "```{ext}\n{text}\n```"
        )))
    }
}
