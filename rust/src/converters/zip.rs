use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

const MIMETYPES: &[&str] = &["application/zip", "application/x-zip-compressed"];

/// Extracts a ZIP archive and converts each entry with the parent converters.
pub struct ZipConverter {
    parent_converters: Arc<Vec<Box<dyn Converter>>>,
}

impl ZipConverter {
    pub fn new(parent_converters: Arc<Vec<Box<dyn Converter>>>) -> Self {
        Self { parent_converters }
    }
}

impl Converter for ZipConverter {
    fn name(&self) -> &'static str {
        "zip"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        matches!(info.extension.as_deref(), Some(".zip"))
            || info
                .mimetype
                .as_deref()
                .is_some_and(|m| MIMETYPES.iter().any(|p| m.starts_with(p)))
    }

    fn convert(
        &self,
        input: &[u8],
        info: &StreamInfo,
        options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(input))?;
        let label = info
            .local_path
            .as_deref()
            .or(info.filename.as_deref())
            .unwrap_or("archive.zip");
        let base = Path::new(label)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| label.to_string());

        let mut sections: Vec<String> = vec![format!("Content from `{base}`:")];

        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            if file.is_dir() {
                continue;
            }
            let path = file.name().to_string();
            let file_info = StreamInfo {
                extension: Path::new(&path)
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy().to_lowercase())),
                filename: Path::new(&path)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned()),
                ..Default::default()
            };

            let mut buffer = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut buffer)?;
            drop(file);

            let mut converted = false;
            for converter in self.parent_converters.iter() {
                if converter.name() == "zip" {
                    continue; // avoid recursion loops
                }
                if !converter.accepts(&file_info) {
                    continue;
                }
                if let Ok(result) = converter.convert(&buffer, &file_info, options) {
                    let md = result.markdown.trim().to_string();
                    if !md.is_empty() {
                        sections.push(format!("## File: {path}\n\n{md}"));
                        converted = true;
                        break;
                    }
                }
            }

            if !converted {
                sections.push(format!("## File: {path}\n\n*[binary file]*"));
            }
        }

        Ok(ConversionResult::markdown(
            sections.join("\n\n").trim().to_string(),
        ))
    }
}
