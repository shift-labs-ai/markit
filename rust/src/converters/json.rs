use anyhow::Result;

use super::decode_text;
use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

pub struct JsonConverter;

impl Converter for JsonConverter {
    fn name(&self) -> &'static str {
        "json"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        matches!(info.extension.as_deref(), Some(".json"))
            || info
                .mimetype
                .as_deref()
                .is_some_and(|m| m.starts_with("application/json"))
    }

    fn convert(
        &self,
        input: &[u8],
        _info: &StreamInfo,
        _options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let parsed: serde_json::Value = serde_json::from_str(&decode_text(input))?;
        let pretty = serde_json::to_string_pretty(&parsed)?;
        Ok(ConversionResult::markdown(format!(
            "```json\n{pretty}\n```"
        )))
    }
}
