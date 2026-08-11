//! PDF to Markdown converter.
//!
//! Pipeline:
//!   1. Extract text boxes + vector segments + image regions with the own engine
//!   2. Strip running headers/footers
//!   3. Detect column layout (single vs multi-column)
//!   4. Per column: detect table grids from segments
//!   5. Render diagrams as PNG files (if output directory provided)
//!   6. Render tables as markdown tables, free text as paragraphs/headings

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::types::{ConversionResult, Converter, StreamInfo};

use super::columns::detect_columns;
use super::fast_extract::extract_pages_fast;
use super::grid::resolve_table_grids;
use super::headers::{strip_headers_footers, strip_single_page_chrome};
use super::render::{render_page_content, ImageBlock};
use super::types::{Segment, TextBox};

const EXTENSIONS: &[&str] = &[".pdf"];
const MIMETYPES: &[&str] = &["application/pdf", "application/x-pdf"];

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

fn image_blocks_in_x_range(
    blocks: &[(f64, ImageBlock)],
    min_x: f64,
    max_x: f64,
) -> Vec<ImageBlock> {
    blocks
        .iter()
        .filter(|(center_x, _)| *center_x >= min_x && *center_x <= max_x)
        .map(|(_, block)| block.clone())
        .collect()
}

pub struct PdfConverter;

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

    fn convert(&self, input: &[u8], info: &StreamInfo) -> Result<ConversionResult> {
        let mut pages = extract_pages_fast(input)?;

        // Remove running headers/footers before processing. The
        // repetition detector handles multi-page documents; the per-page
        // chrome detector complements it on single pages and
        // inconsistent per-page chrome.
        strip_headers_footers(&mut pages);
        strip_single_page_chrome(&mut pages);

        let image_dir = info.image_dir.as_deref();
        if let Some(dir) = image_dir {
            fs::create_dir_all(dir)?;
        }

        let mut page_markdowns: Vec<String> = Vec::new();

        for page in &pages {
            // Build image blocks for this page
            let mut positioned_image_blocks: Vec<(f64, ImageBlock)> = Vec::new();

            if let Some(dir) = image_dir {
                if !page.images.is_empty() {
                    for img in &page.images {
                        // Preserve the embedded image at native resolution
                        // when its encoding is supported by the own engine.
                        let written =
                            match super::image_extract::extract_image_region_fast(input, img) {
                                Ok(x) => {
                                    let filepath =
                                        Path::new(dir).join(format!("{}.{}", img.id, x.ext));
                                    match fs::write(&filepath, &x.bytes) {
                                        Ok(()) => Some(filepath),
                                        Err(e) => {
                                            eprintln!(
                                                "Failed to write image {}: {}",
                                                filepath.display(),
                                                e
                                            );
                                            None
                                        }
                                    }
                                }
                                Err(_) => None,
                            };
                        if let Some(filepath) = written {
                            positioned_image_blocks.push((
                                img.bbox.x + img.bbox.w / 2.0,
                                ImageBlock {
                                    top_y: img.top_y,
                                    markdown: format!("![{}]({})", img.id, filepath.display()),
                                },
                            ));
                            continue;
                        }
                    }
                }
            } else if !page.images.is_empty() {
                for img in &page.images {
                    positioned_image_blocks.push((
                        img.bbox.x + img.bbox.w / 2.0,
                        ImageBlock {
                            top_y: img.top_y,
                            markdown: format!(
                                "<!-- image: {} (page {}, {}x{}pt) -->",
                                img.id, img.page_number, img.bbox.w as i32, img.bbox.h as i32
                            ),
                        },
                    ));
                }
            }
            let image_blocks: Vec<ImageBlock> = positioned_image_blocks
                .iter()
                .map(|(_, block)| block.clone())
                .collect();

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
                for col_boxes in &layout.columns {
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

                    let column_image_blocks = image_blocks_in_x_range(
                        &positioned_image_blocks,
                        col_x_min - margin,
                        col_x_max + margin,
                    );

                    let md = process_column(
                        page.page_number,
                        col_boxes,
                        &col_segments,
                        &column_image_blocks,
                    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn images_are_assigned_to_their_own_text_column() {
        let blocks = vec![
            (
                100.0,
                ImageBlock {
                    top_y: 500.0,
                    markdown: "left".into(),
                },
            ),
            (
                400.0,
                ImageBlock {
                    top_y: 500.0,
                    markdown: "right".into(),
                },
            ),
        ];
        let right = image_blocks_in_x_range(&blocks, 300.0, 500.0);
        assert_eq!(right.len(), 1);
        assert_eq!(right[0].markdown, "right");
    }

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
        let input = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >> endobj
4 0 obj << /Length 0 >> stream

endstream endobj
trailer << /Root 1 0 R >>";
        let c = PdfConverter;
        let info = StreamInfo {
            extension: Some(".pdf".into()),
            ..Default::default()
        };
        assert!(c.convert(input, &info).is_ok());
    }

    #[test]
    fn full_pipeline_with_fixture() {
        let fixture = "../test/fixtures/pdfs/intel-743621-007.pdf";
        assert!(Path::new(fixture).exists(), "committed fixture missing");

        let buf = std::fs::read(fixture).unwrap();
        let c = PdfConverter;
        let info = StreamInfo {
            extension: Some(".pdf".into()),
            local_path: Some(fixture.into()),
            ..Default::default()
        };
        let result = c.convert(&buf, &info).unwrap();

        assert!(!result.markdown.is_empty());
        assert!(
            result.markdown.contains("700 Series")
                || result.markdown.contains("Platform Controller Hub"),
            "Output should contain key text from the PDF"
        );
    }
}
