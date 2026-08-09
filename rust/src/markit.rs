use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::converters::csv::CsvConverter;
use crate::converters::docx::DocxConverter;
use crate::converters::epub::EpubConverter;
use crate::converters::html::HtmlConverter;
use crate::converters::iwork::IWorkConverter;
use crate::converters::json::JsonConverter;
use crate::converters::plain_text::PlainTextConverter;
use crate::converters::pptx::PptxConverter;
use crate::converters::xlsx::XlsxConverter;
use crate::converters::xml::XmlConverter;
use crate::converters::yaml::YamlConverter;
use crate::converters::zip::ZipConverter;
use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

pub struct Markit {
    converters: Vec<Box<dyn Converter>>,
    options: MarkitOptions,
}

impl Markit {
    pub fn new(options: MarkitOptions) -> Self {
        // Specific formats first, generic last, plain text as catch-all.
        // Mirrors the ordering in src/markit.ts.
        let specific_and_generic: Arc<Vec<Box<dyn Converter>>> = Arc::new(vec![
            Box::new(DocxConverter),
            Box::new(PptxConverter),
            Box::new(XlsxConverter),
            Box::new(EpubConverter),
            Box::new(IWorkConverter),
            Box::new(CsvConverter),
            Box::new(JsonConverter),
            Box::new(YamlConverter),
            Box::new(XmlConverter),
            Box::new(HtmlConverter),
        ]);

        // ZIP gets the other converters for recursive extraction.
        let zip = ZipConverter::new(Arc::clone(&specific_and_generic));

        let mut converters: Vec<Box<dyn Converter>> = vec![
            Box::new(DocxConverter),
            Box::new(PptxConverter),
            Box::new(XlsxConverter),
            Box::new(EpubConverter),
            Box::new(IWorkConverter),
            Box::new(CsvConverter),
            Box::new(JsonConverter),
            Box::new(YamlConverter),
            Box::new(zip),
            Box::new(XmlConverter),
            Box::new(HtmlConverter),
        ];
        converters.push(Box::new(PlainTextConverter));
        Self { converters, options }
    }

    /// Convert a local file to markdown.
    pub fn convert_file(&self, path: &str) -> Result<ConversionResult> {
        let buffer = fs::read(path)?;
        let p = Path::new(path);
        let info = StreamInfo {
            local_path: Some(path.to_string()),
            extension: p
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_lowercase())),
            filename: p.file_name().map(|f| f.to_string_lossy().into_owned()),
            ..Default::default()
        };
        self.convert(&buffer, &info)
    }

    /// Convert a buffer with stream info to markdown.
    pub fn convert(&self, input: &[u8], info: &StreamInfo) -> Result<ConversionResult> {
        let mut errors: Vec<(&'static str, anyhow::Error)> = Vec::new();

        for converter in &self.converters {
            if !converter.accepts(info) {
                continue;
            }
            match converter.convert(input, info, &self.options) {
                Ok(result) => return Ok(result),
                Err(err) => errors.push((converter.name(), err)),
            }
        }

        if !errors.is_empty() {
            let details = errors
                .iter()
                .map(|(name, err)| format!("  {name}: {err}"))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(anyhow!("Conversion failed:\n{details}"));
        }

        Err(anyhow!(
            "Unsupported format: {}",
            info.extension
                .as_deref()
                .or(info.mimetype.as_deref())
                .unwrap_or("unknown")
        ))
    }
}
