//! PDF to Markdown converter.
//!
//! Pipeline:
//!   1. Extract text boxes + vector segments + image regions per page (mupdf)
//!   2. Strip running headers/footers
//!   3. Detect column layout (single vs multi-column)
//!   4. Per column: detect table grids from segments
//!   5. Render diagrams as PNG files (if output directory provided)
//!   6. Render tables as markdown tables, free text as paragraphs/headings

#[cfg(feature = "pdf")]
use std::collections::HashSet;
#[cfg(feature = "pdf")]
use std::fs;
#[cfg(feature = "pdf")]
use std::path::Path;

use anyhow::Result;

use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

#[cfg(feature = "pdf")]
use super::columns::detect_columns;
#[cfg(feature = "pdf")]
use super::extract::{extract_pages, render_image_region};
#[cfg(feature = "pdf")]
use super::grid::resolve_table_grids;
#[cfg(feature = "pdf")]
use super::headers::strip_headers_footers;
#[cfg(feature = "pdf")]
use super::render::{render_page_content, ImageBlock};
#[cfg(feature = "pdf")]
use super::types::{Segment, TextBox};

const EXTENSIONS: &[&str] = &[".pdf"];
const MIMETYPES: &[&str] = &["application/pdf", "application/x-pdf"];

#[cfg(feature = "pdf")]
/// Process a set of text boxes (one column or full page): run table detection,
/// separate free text, and render to markdown.
fn process_column(
    page_number: u32,
    text_boxes: &[TextBox],
    segments: &[Segment],
    image_blocks: &[ImageBlock],
) -> String {
    let result = resolve_table_grids(page_number, text_boxes, segments);

    let consumed_set: HashSet<&str> = result.consumed_ids.iter().map(|s| s.as_str()).collect();
    let free_text_boxes: Vec<TextBox> = text_boxes
        .iter()
        .filter(|tb| !consumed_set.contains(tb.id.as_str()))
        .cloned()
        .collect();

    render_page_content(
        &free_text_boxes,
        &result.grids,
        image_blocks,
        Some(text_boxes),
    )
}

pub struct PdfConverter;

#[cfg(feature = "pdf")]
impl Converter for PdfConverter {
    fn name(&self) -> &'static str {
        "pdf"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ext) = &info.extension {
            if EXTENSIONS.contains(&ext.as_str()) {
                return true;
            }
        }
        if let Some(mime) = &info.mimetype {
            if MIMETYPES.iter().any(|m| mime.starts_with(m)) {
                return true;
            }
        }
        false
    }

    fn convert(
        &self,
        input: &[u8],
        info: &StreamInfo,
        options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let mut pages = extract_pages(input)?;

        // Remove running headers/footers before processing
        strip_headers_footers(&mut pages);

        let image_dir = info.image_dir.as_deref();
        if let Some(dir) = image_dir {
            fs::create_dir_all(dir)?;
        }

        let mut page_markdowns: Vec<String> = Vec::new();

        for page in &pages {
            // Build image blocks for this page
            let mut image_blocks: Vec<ImageBlock> = Vec::new();

            if let Some(dir) = image_dir {
                if !page.images.is_empty() {
                    for img in &page.images {
                        let filename = format!("{}.png", img.id);
                        let filepath = Path::new(dir).join(&filename);
                        match render_image_region(input, img) {
                            Ok(png) => {
                                if let Err(e) = fs::write(&filepath, &png) {
                                    eprintln!(
                                        "Failed to write image {}: {}",
                                        filepath.display(),
                                        e
                                    );
                                    continue;
                                }

                                // If describe callback is available, call it
                                let markdown = if let Some(describe) = &options.describe {
                                    match describe(&png, "image/png") {
                                        Ok(desc) => desc,
                                        Err(_) => {
                                            format!("![{}]({})", img.id, filepath.display())
                                        }
                                    }
                                } else {
                                    format!("![{}]({})", img.id, filepath.display())
                                };

                                image_blocks.push(ImageBlock {
                                    top_y: img.top_y,
                                    markdown,
                                });
                            }
                            Err(_) => {
                                // Image rendering failed — skip
                            }
                        }
                    }
                }
            } else if !page.images.is_empty() {
                for img in &page.images {
                    image_blocks.push(ImageBlock {
                        top_y: img.top_y,
                        markdown: format!(
                            "<!-- image: {} (page {}, {}x{}pt) -->",
                            img.id, img.page_number, img.bbox.w as i32, img.bbox.h as i32
                        ),
                    });
                }
            }

            // Detect column layout
            let mut layout = detect_columns(&page.text_boxes);

            // If the page has vertical segments (tables), suppress column detection
            // when one detected column is very narrow
            if layout.column_count > 1
                && page.segments.iter().any(|s| (s.x1 - s.x2).abs() <= 0.8)
                && !page.text_boxes.is_empty()
            {
                let page_x_min = page
                    .text_boxes
                    .iter()
                    .map(|tb| tb.bounds.left)
                    .fold(f64::INFINITY, f64::min);
                let page_x_max = page
                    .text_boxes
                    .iter()
                    .map(|tb| tb.bounds.right)
                    .fold(f64::NEG_INFINITY, f64::max);
                let page_width = page_x_max - page_x_min;
                let min_col_fraction = 0.3;

                let too_narrow = layout.columns.iter().any(|col| {
                    if col.is_empty() {
                        return true;
                    }
                    let col_x_min = col
                        .iter()
                        .map(|tb| tb.bounds.left)
                        .fold(f64::INFINITY, f64::min);
                    let col_x_max = col
                        .iter()
                        .map(|tb| tb.bounds.right)
                        .fold(f64::NEG_INFINITY, f64::max);
                    (col_x_max - col_x_min) / page_width < min_col_fraction
                });

                if too_narrow {
                    layout.column_count = 1;
                    layout.columns = vec![page.text_boxes.clone()];
                    layout.boundaries = Vec::new();
                }
            }

            if layout.column_count == 1 {
                let md = process_column(
                    page.page_number,
                    &page.text_boxes,
                    &page.segments,
                    &image_blocks,
                );
                if !md.is_empty() {
                    page_markdowns.push(md);
                }
            } else {
                let mut column_markdowns: Vec<String> = Vec::new();
                for (col_idx, col_boxes) in layout.columns.iter().enumerate() {
                    // Filter segments to those within this column's X range
                    let col_x_min = col_boxes
                        .iter()
                        .map(|tb| tb.bounds.left)
                        .fold(f64::INFINITY, f64::min);
                    let col_x_max = col_boxes
                        .iter()
                        .map(|tb| tb.bounds.right)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let margin = 10.0;

                    let col_segments: Vec<Segment> = page
                        .segments
                        .iter()
                        .filter(|seg| {
                            let seg_x_min = seg.x1.min(seg.x2);
                            let seg_x_max = seg.x1.max(seg.x2);
                            seg_x_max >= col_x_min - margin && seg_x_min <= col_x_max + margin
                        })
                        .cloned()
                        .collect();

                    // Images go with the first column only
                    let blocks = if col_idx == 0 { &image_blocks[..] } else { &[] };

                    let md = process_column(page.page_number, col_boxes, &col_segments, blocks);
                    if !md.is_empty() {
                        column_markdowns.push(md);
                    }
                }

                let joined = column_markdowns.join("\n\n");
                if !joined.is_empty() {
                    page_markdowns.push(joined);
                }
            }
        }

        Ok(ConversionResult::markdown(page_markdowns.join("\n\n")))
    }
}

#[cfg(all(test, feature = "pdf"))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn accepts_pdf_extension() {
        let c = PdfConverter;
        assert!(c.accepts(&StreamInfo {
            extension: Some(".pdf".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn accepts_pdf_mimetype() {
        let c = PdfConverter;
        assert!(c.accepts(&StreamInfo {
            mimetype: Some("application/pdf".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn rejects_non_pdf() {
        let c = PdfConverter;
        assert!(!c.accepts(&StreamInfo {
            extension: Some(".docx".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn converts_generated_pdf() {
        use mupdf::pdf::PdfDocument;

        let mut doc = PdfDocument::new();
        match doc.new_page(mupdf::Size {
            width: 612.0,
            height: 792.0,
        }) {
            Ok(_) => {
                let mut buf = Vec::new();
                match doc.write_to(&mut buf) {
                    Ok(_) => {
                        let c = PdfConverter;
                        let info = StreamInfo {
                            extension: Some(".pdf".into()),
                            ..Default::default()
                        };
                        let opts = MarkitOptions::default();
                        let result = c.convert(&buf, &info, &opts);
                        assert!(result.is_ok());
                    }
                    Err(e) => {
                        eprintln!("Could not write PDF: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Could not create page: {}", e);
            }
        }
    }

    #[test]
    fn full_pipeline_with_fixture() {
        let fixture = "../test/fixtures/pdfs/intel-743621-007.pdf";
        if !Path::new(fixture).exists() {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture).unwrap();
        let c = PdfConverter;
        let info = StreamInfo {
            extension: Some(".pdf".into()),
            local_path: Some(fixture.into()),
            ..Default::default()
        };
        let opts = MarkitOptions::default();
        let result = c.convert(&buf, &info, &opts).unwrap();

        assert!(!result.markdown.is_empty());
        assert!(
            result.markdown.contains("700 Series")
                || result.markdown.contains("Platform Controller Hub"),
            "Output should contain key text from the PDF"
        );
    }
}

/// Fallback when built without the "pdf" feature: accepts PDFs but fails
/// with build instructions — mirrors the TS dynamic-import error
/// ("PDF support requires 'mupdf'. Install it: npm install mupdf").
#[cfg(not(feature = "pdf"))]
impl Converter for PdfConverter {
    fn name(&self) -> &'static str {
        "pdf"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ext) = &info.extension {
            if EXTENSIONS.contains(&ext.as_str()) {
                return true;
            }
        }
        if let Some(mime) = &info.mimetype {
            if MIMETYPES.iter().any(|m| mime.starts_with(m)) {
                return true;
            }
        }
        false
    }

    fn convert(
        &self,
        _input: &[u8],
        _info: &StreamInfo,
        _options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        Err(anyhow::anyhow!(
            "PDF support requires the 'pdf' feature (MuPDF). Rebuild with: cargo build --features pdf"
        ))
    }
}
