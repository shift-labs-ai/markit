use anyhow::Result;

use super::decode_text;
use crate::types::{ConversionResult, Converter, StreamInfo};

const TEXT_EXTENSIONS: &[&str] = &[
    ".txt",
    ".md",
    ".markdown",
    ".rst",
    ".log",
    ".vtt",
    ".srt",
    ".cfg",
    ".ini",
    ".yaml",
    ".yml",
    ".toml",
    ".xml",
    ".svg",
    ".env",
    ".sh",
    ".bash",
    ".zsh",
    ".fish",
    ".py",
    ".js",
    ".ts",
    ".jsx",
    ".tsx",
    ".go",
    ".rs",
    ".rb",
    ".java",
    ".c",
    ".cpp",
    ".h",
    ".hpp",
    ".cs",
    ".swift",
    ".kt",
    ".scala",
    ".sql",
    ".r",
    ".m",
    ".lua",
    ".pl",
    ".php",
    ".ex",
    ".exs",
    ".zig",
    ".nim",
    ".v",
    ".d",
    ".hs",
    ".ml",
    ".clj",
    ".makefile",
    ".dockerfile",
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

    fn convert(&self, input: &[u8], info: &StreamInfo) -> Result<ConversionResult> {
        let text = decode_text(input);
        let ext = info.extension.as_deref();

        // If it's already markdown, return as-is.
        if matches!(ext, Some(".md") | Some(".markdown")) {
            return Ok(ConversionResult::markdown(text));
        }

        // For code files, wrap in a fenced code block.
        if let Some(lang) = ext.map(|e| e.trim_start_matches('.')) {
            if !lang.is_empty() && !matches!(lang, "txt" | "log" | "rst" | "vtt" | "srt") {
                return Ok(ConversionResult::markdown(format!(
                    "```{lang}\n{text}\n```"
                )));
            }
        }

        Ok(ConversionResult::markdown(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(ext: &str, text: &str) -> String {
        let info = StreamInfo {
            extension: Some(ext.to_string()),
            ..Default::default()
        };
        assert!(PlainTextConverter.accepts(&info), "{ext}");
        PlainTextConverter
            .convert(text.as_bytes(), &info)
            .unwrap()
            .markdown
    }

    #[test]
    fn captions_pass_through_unchanged() {
        let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nHello world.\n";
        let srt = "1\n00:00:00,000 --> 00:00:02,000\nHello world.\n";
        assert_eq!(convert(".vtt", vtt), vtt);
        assert_eq!(convert(".srt", srt), srt);
    }

    #[test]
    fn code_is_fenced_with_its_language() {
        assert_eq!(convert(".py", "print(1)"), "```py\nprint(1)\n```");
    }
}
