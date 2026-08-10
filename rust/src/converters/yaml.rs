use anyhow::Result;

use super::decode_text;
use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

const MIMETYPES: &[&str] = &["text/yaml", "application/x-yaml"];

pub struct YamlConverter;

impl Converter for YamlConverter {
    fn name(&self) -> &'static str {
        "yaml"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        matches!(info.extension.as_deref(), Some(".yaml") | Some(".yml"))
            || info
                .mimetype
                .as_deref()
                .is_some_and(|m| MIMETYPES.iter().any(|p| m.starts_with(p)))
    }

    fn convert(
        &self,
        input: &[u8],
        _info: &StreamInfo,
        _options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let text = decode_text(input);
        Ok(ConversionResult::markdown(format!("```yaml\n{text}\n```")))
    }
}
