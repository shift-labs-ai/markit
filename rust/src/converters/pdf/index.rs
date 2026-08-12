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

use super::borderless::detect_borderless_tables;
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

    // Tables without any ruling leave nothing for segment detection:
    // reconstruct them from text alignment among the remaining boxes.
    let (borderless_grids, borderless_consumed) =
        detect_borderless_tables(&free_text_boxes, page_number);
    let borderless_set: HashSet<&str> = borderless_consumed.iter().map(|s| s.as_str()).collect();
    let free_text_boxes: Vec<TextBox> = free_text_boxes
        .into_iter()
        .filter(|tb| !borderless_set.contains(tb.id.as_str()))
        .collect();
    let mut grids = result.grids;
    grids.extend(borderless_grids);

    render_page_content(&free_text_boxes, &grids, image_blocks, Some(text_boxes))
}

/// Join hyphenated words split across block boundaries — column ends,
/// region flushes, page breaks. Per-group paragraph merging can never
/// see these: by render time the continuation lives in another block.
/// `tech-\n\nniques` → `techniques`; a compound (`state-of-the-` +
/// `art`) keeps its hyphen. Only fires when a letter precedes the
/// hyphen and a lowercase letter opens the next block.
fn dehyphenate_across_blocks(markdown: &str) -> String {
    let bytes = markdown.as_bytes();
    let mut out = String::with_capacity(markdown.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' && bytes[i + 1..].starts_with(b"\n\n") {
            let before_alpha = out.chars().last().is_some_and(|c| c.is_alphabetic());
            let next = markdown[i + 3..].chars().next();
            let next_lower = next.is_some_and(|c| c.is_lowercase() && c.is_alphabetic());
            if before_alpha && next_lower {
                let last_word: String = out
                    .chars()
                    .rev()
                    .take_while(|c| !c.is_whitespace())
                    .collect();
                if last_word.contains('-') {
                    // Compound: keep the hyphen, drop the break.
                    out.push('-');
                }
                i += 3; // skip "-\n\n"
                continue;
            }
        }
        let ch = markdown[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// A group's bounding extents (with margin) for content assignment.
struct GroupExtent {
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
}

fn group_extent(boxes: &[TextBox], margin: f64) -> GroupExtent {
    GroupExtent {
        x_min: boxes
            .iter()
            .map(|tb| tb.bounds.left)
            .fold(f64::INFINITY, f64::min)
            - margin,
        x_max: boxes
            .iter()
            .map(|tb| tb.bounds.right)
            .fold(f64::NEG_INFINITY, f64::max)
            + margin,
        y_min: boxes
            .iter()
            .map(|tb| tb.bounds.bottom)
            .fold(f64::INFINITY, f64::min)
            - margin,
        y_max: boxes
            .iter()
            .map(|tb| tb.bounds.top)
            .fold(f64::NEG_INFINITY, f64::max)
            + margin,
    }
}

/// Assign each image to exactly one group: the x-containing group with
/// the smallest vertical distance. Unclaimed images fall to the group
/// with the nearest vertical distance regardless of x.
fn assign_images_to_groups(
    blocks: &[(f64, ImageBlock)],
    extents: &[GroupExtent],
) -> Vec<Vec<ImageBlock>> {
    let mut assigned: Vec<Vec<ImageBlock>> = vec![Vec::new(); extents.len()];
    for (center_x, block) in blocks {
        let mut best: Option<(usize, f64, bool)> = None; // (group, dy, x_match)
        for (gi, extent) in extents.iter().enumerate() {
            let x_match = *center_x >= extent.x_min && *center_x <= extent.x_max;
            let dy = if block.top_y > extent.y_max {
                block.top_y - extent.y_max
            } else if block.top_y < extent.y_min {
                extent.y_min - block.top_y
            } else {
                0.0
            };
            let better = match best {
                None => true,
                Some((_, best_dy, best_x)) => {
                    (x_match && !best_x) || (x_match == best_x && dy < best_dy)
                }
            };
            if better {
                best = Some((gi, dy, x_match));
            }
        }
        if let Some((gi, _, _)) = best {
            assigned[gi].push(block.clone());
        }
    }
    assigned
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
            let mut layout = detect_columns(&page.text_boxes, &page.segments);

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
                // A legitimate column spans roughly page_width/n; the
                // 30% floor assumed two columns and vetoed every
                // four-column spread (each column ~22%). Scale with the
                // detected gutter count.
                let columns_detected = (layout.boundaries.len() + 1).max(2) as f64;
                let min_col_fraction = 0.6 / columns_detected;

                // Bands (full-width titles/headings) are legitimately
                // narrow or wide; only substantive column groups vote —
                // a stray page number or caption is not a mis-split
                // table column.
                let too_narrow = layout.columns.iter().zip(&layout.bands).any(|(col, band)| {
                    if *band || col.len() < 4 {
                        return false;
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
                    layout.bands = vec![false];
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
                let margin = 10.0;
                let extents: Vec<GroupExtent> = layout
                    .columns
                    .iter()
                    .map(|col| group_extent(col, margin))
                    .collect();
                let group_images = assign_images_to_groups(&positioned_image_blocks, &extents);

                let mut column_markdowns: Vec<String> = Vec::new();
                for (gi, col_boxes) in layout.columns.iter().enumerate() {
                    let extent = &extents[gi];
                    // Segments must overlap the group in BOTH axes —
                    // stacked slices share x-ranges, and an x-only filter
                    // would duplicate table rules into every slice.
                    let col_segments: Vec<Segment> = page
                        .segments
                        .iter()
                        .filter(|seg| {
                            let seg_x_min = seg.x1.min(seg.x2);
                            let seg_x_max = seg.x1.max(seg.x2);
                            let seg_y_min = seg.y1.min(seg.y2);
                            let seg_y_max = seg.y1.max(seg.y2);
                            seg_x_max >= extent.x_min
                                && seg_x_min <= extent.x_max
                                && seg_y_max >= extent.y_min
                                && seg_y_min <= extent.y_max
                        })
                        .cloned()
                        .collect();

                    let md = process_column(
                        page.page_number,
                        col_boxes,
                        &col_segments,
                        &group_images[gi],
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

        Ok(ConversionResult::markdown(dehyphenate_across_blocks(
            &page_markdowns.join("\n\n"),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn extent(x_min: f64, x_max: f64, y_min: f64, y_max: f64) -> GroupExtent {
        GroupExtent {
            x_min,
            x_max,
            y_min,
            y_max,
        }
    }

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
        let extents = vec![
            extent(50.0, 250.0, 100.0, 700.0),
            extent(300.0, 500.0, 100.0, 700.0),
        ];
        let assigned = assign_images_to_groups(&blocks, &extents);
        assert_eq!(assigned[0].len(), 1);
        assert_eq!(assigned[0][0].markdown, "left");
        assert_eq!(assigned[1].len(), 1);
        assert_eq!(assigned[1][0].markdown, "right");
    }

    #[test]
    fn stacked_slices_claim_images_by_vertical_distance_without_duplication() {
        let blocks = vec![(
            300.0,
            ImageBlock {
                top_y: 500.0,
                markdown: "figure".into(),
            },
        )];
        // Two full-width stacked slices; the image sits in the upper one.
        let extents = vec![
            extent(50.0, 550.0, 450.0, 700.0),
            extent(50.0, 550.0, 100.0, 400.0),
        ];
        let assigned = assign_images_to_groups(&blocks, &extents);
        assert_eq!(assigned[0].len(), 1);
        assert_eq!(assigned[1].len(), 0);
    }

    #[test]
    fn dehyphenates_across_block_boundaries() {
        assert_eq!(
            dehyphenate_across_blocks("knowledge-based tech-\n\nniques applied"),
            "knowledge-based techniques applied"
        );
        // Compound keeps its hyphen.
        assert_eq!(
            dehyphenate_across_blocks("the state-of-the-\n\nart system"),
            "the state-of-the-art system"
        );
        // Uppercase continuation is a new sentence/heading: untouched.
        assert_eq!(
            dehyphenate_across_blocks("ends with dash-\n\nNew block"),
            "ends with dash-\n\nNew block"
        );
        // List markers are not hyphenated words.
        assert_eq!(
            dehyphenate_across_blocks("a list:\n\n- item"),
            "a list:\n\n- item"
        );
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
        let fixture = "testdata/pdf/regression/09f90a8fad0997f7cf454cbcbe79cab3bc0f_page_1_pg1.pdf";
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
            result
                .markdown
                .contains("Open-Domain Textual Question Answering"),
            "Output should contain key text from the PDF"
        );
    }
}
