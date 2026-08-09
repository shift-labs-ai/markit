use anyhow::Result;

use super::decode_text;
use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

const TEXT_EXTENSIONS: &[&str] = &[
    ".txt", ".md", ".markdown", ".rst", ".log", ".cfg", ".ini", ".yaml", ".yml", ".toml", ".xml",
    ".svg", ".env", ".sh", ".bash", ".zsh", ".fish", ".py", ".js", ".ts", ".jsx", ".tsx", ".go",
    ".rs", ".rb", ".java", ".c", ".cpp", ".h", ".hpp", ".cs", ".swift", ".kt", ".scala", ".sql",
    ".r", ".m", ".lua", ".pl", ".php", ".ex", ".exs", ".zig", ".nim", ".v", ".d", ".hs", ".ml",
    ".clj", ".makefile", ".dockerfile",
];

pub struct PlainTextConverter;

impl Converter for PlainTextConverter {
    fn name(&self) -> &'static str {
        "plain-text"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ext) = &info.extension {
            if TEXT_EXTENSIONS.contains(&ext.as_str()) {
                return true;
            }
        }
        if let Some(mime) = &info.mimetype {
            if mime.starts_with("text/") {
                return true;
            }
        }
        // If nothing else matched and there's no extension, try to decode as text.
        info.extension.is_none() && info.mimetype.is_none()
    }

    fn convert(
        &self,
        input: &[u8],
        info: &StreamInfo,
        _options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let text = decode_text(input);
        let ext = info.extension.as_deref();

        // If it's already markdown, return as-is.
        if matches!(ext, Some(".md") | Some(".markdown")) {
            return Ok(ConversionResult::markdown(text));
        }

        // For code files, wrap in a fenced code block.
        if let Some(lang) = ext.map(|e| e.trim_start_matches('.')) {
            if !lang.is_empty() && !matches!(lang, "txt" | "log" | "rst") {
                return Ok(ConversionResult::markdown(format!(
                    "```{lang}\n{text}\n```"
                )));
            }
        }

        Ok(ConversionResult::markdown(text))
    }
}
