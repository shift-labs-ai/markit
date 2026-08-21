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
use super::columns::{detect_columns, ColumnLayout};
use super::fast_extract::extract_document_fast;
use super::grid::resolve_table_grids;
use super::headers::{strip_folio_series, strip_headers_footers, strip_single_page_chrome};
use super::render::{render_page_content, ImageBlock};
use super::types::{Segment, TextBox};

const EXTENSIONS: &[&str] = &[".pdf"];
const MIMETYPES: &[&str] = &["application/pdf", "application/x-pdf"];

/// Opening of a page marker, completed by the 1-based physical page
/// number and ` -->`. An HTML comment so the marker is invisible when
/// the markdown is rendered, and a fixed prefix so a reader can split
/// on it: `<!-- markit:page 12 -->`.
const PAGE_MARKER_PREFIX: &str = "<!-- markit:page ";

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

/// A ruled table can leave one detached value or status group beside an
/// otherwise full-width page. That is not a page column.
///
/// Three gates must all agree before the layout collapses, because a
/// false positive destroys a real multi-column page:
///   1. exactly one gutter, with a vertical rule drawn along it;
///   2. the gutter sits near an edge, or the page is overwhelmingly one
///      full-width band with a detached field beside it;
///   3. one side of the gutter holds at most one group, so the split is
///      not a genuine column pair recurring down the page.
fn is_sparse_ruled_split(layout: &ColumnLayout, segments: &[Segment]) -> bool {
    /// How near the gutter a vertical rule must fall to explain it.
    const RULE_GUTTER_TOLERANCE: f64 = 24.0;
    /// Gutters within this band of the text width are plausible page
    /// columns rather than a detached edge field.
    const CENTRAL_GUTTER_BAND: std::ops::RangeInclusive<f64> = 0.25..=0.75;

    if layout.boundaries.len() != 1 {
        return false;
    }
    let boundary = layout.boundaries[0];
    if !segments.iter().any(|segment| {
        (segment.x1 - segment.x2).abs() <= 0.8
            && ((segment.x1 + segment.x2) / 2.0 - boundary).abs() <= RULE_GUTTER_TOLERANCE
    }) {
        return false;
    }
    let page_left = layout
        .columns
        .iter()
        .flatten()
        .map(|tb| tb.bounds.left)
        .fold(f64::INFINITY, f64::min);
    let page_right = layout
        .columns
        .iter()
        .flatten()
        .map(|tb| tb.bounds.right)
        .fold(f64::NEG_INFINITY, f64::max);
    let boundary_fraction = (boundary - page_left) / (page_right - page_left).max(1.0);
    let total_boxes: usize = layout.columns.iter().map(Vec::len).sum();
    let dominant_band = layout
        .columns
        .iter()
        .zip(&layout.bands)
        .filter(|(_, band)| **band)
        .map(|(boxes, _)| boxes.len())
        .max()
        .is_some_and(|count| count * 5 >= total_boxes * 4);
    if CENTRAL_GUTTER_BAND.contains(&boundary_fraction) && !dominant_band {
        return false;
    }

    let mut left_groups = 0usize;
    let mut right_groups = 0usize;
    for (boxes, is_band) in layout.columns.iter().zip(&layout.bands) {
        if *is_band || boxes.is_empty() {
            continue;
        }
        let extent = group_extent(boxes, 0.0);
        if extent.x_max <= boundary {
            left_groups += 1;
        } else if extent.x_min >= boundary {
            right_groups += 1;
        }
    }
    left_groups <= 1 || right_groups <= 1
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
        let image_dir = info.image_dir.as_deref();
        let (mut pages, extracted_images) = extract_document_fast(input, image_dir.is_some())?;

        // Remove running headers/footers before processing. The
        // repetition detector handles multi-page documents, the folio
        // detector the page-number series, and the per-page chrome
        // detector complements both on single pages and inconsistent
        // per-page chrome. Order matters: each pass narrows what the
        // next one is allowed to call chrome.
        strip_headers_footers(&mut pages);
        strip_folio_series(&mut pages);
        strip_single_page_chrome(&mut pages);

        if let Some(dir) = image_dir {
            fs::create_dir_all(dir)?;
        }

        let mut page_markdowns: Vec<String> = Vec::new();

        for page in &pages {
            // One marker per physical page, ahead of that page's
            // content, so a caller can map any line back to the page it
            // came from. Emitted before the empty-content check: a blank
            // or image-only page still occupies a page number, and a
            // marker sequence with holes in it would be worse than none.
            if info.page_markers {
                page_markdowns.push(format!("{PAGE_MARKER_PREFIX}{} -->", page.page_number));
            }

            // Build image blocks for this page
            let mut positioned_image_blocks: Vec<(f64, ImageBlock)> = Vec::new();

            if let Some(dir) = image_dir {
                if !page.images.is_empty() {
                    let page_images = extracted_images.get(&page.page_number);
                    for (idx, img) in page.images.iter().enumerate() {
                        // Regions and extracted placements share this
                        // area-filtered order by construction.
                        let image = page_images
                            .and_then(|images| images.get(idx))
                            .and_then(|slot| slot.as_ref());
                        let written = match image {
                            Some(x) => {
                                let filepath = Path::new(dir).join(format!("{}.{}", img.id, x.ext));
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
                            None => None,
                        };
                        if let Some(filepath) = written {
                            positioned_image_blocks.push((
                                img.bbox.x + img.bbox.w / 2.0,
                                ImageBlock {
                                    top_y: img.top_y,
                                    markdown: format!("![{}]({})", img.id, filepath.display()),
                                },
                            ));
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
            // Detect column layout. A one-off group detached by a ruled
            // table separator is not a page column.
            let mut layout = detect_columns(&page.text_boxes, &page.segments);
            if is_sparse_ruled_split(&layout, &page.segments) {
                layout = ColumnLayout {
                    column_count: 1,
                    columns: vec![page.text_boxes.clone()],
                    bands: vec![false],
                    boundaries: Vec::new(),
                };
            }

            if layout.column_count == 1 {
                let image_blocks: Vec<ImageBlock> = positioned_image_blocks
                    .into_iter()
                    .map(|(_, block)| block)
                    .collect();
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
    use crate::converters::pdf::types::Bounds;
    use std::path::Path;

    fn layout_box(id: &str, left: f64, right: f64) -> TextBox {
        TextBox {
            id: id.into(),
            text: id.into(),
            page_number: 1,
            font_size: 10.0,
            is_bold: false,
            bounds: Bounds {
                left,
                right,
                top: 700.0,
                bottom: 690.0,
            },
        }
    }

    fn extent(x_min: f64, x_max: f64, y_min: f64, y_max: f64) -> GroupExtent {
        GroupExtent {
            x_min,
            x_max,
            y_min,
            y_max,
        }
    }

    #[test]
    fn sparse_ruled_edge_field_is_not_a_page_column() {
        let segments = vec![Segment {
            id: "rule".into(),
            x1: 400.0,
            y1: 100.0,
            x2: 400.0,
            y2: 300.0,
        }];
        let edge_field = ColumnLayout {
            column_count: 3,
            columns: vec![
                vec![layout_box("left-1", 0.0, 300.0)],
                vec![layout_box("left-2", 0.0, 300.0)],
                vec![layout_box("status", 420.0, 500.0)],
            ],
            bands: vec![false, false, false],
            boundaries: vec![400.0],
        };
        assert!(is_sparse_ruled_split(&edge_field, &segments));

        let balanced_page = ColumnLayout {
            boundaries: vec![250.0],
            ..edge_field
        };
        assert!(!is_sparse_ruled_split(&balanced_page, &segments));
    }

    #[test]
    fn centered_rule_between_real_columns_is_not_a_sparse_split() {
        // Journals draw a hairline down the gutter. Collapsing on that
        // would interleave a genuine two-column page, so the central
        // band must win over the rule.
        let segments = vec![Segment {
            id: "gutter-rule".into(),
            x1: 300.0,
            y1: 100.0,
            x2: 300.0,
            y2: 700.0,
        }];
        let two_column = ColumnLayout {
            column_count: 2,
            columns: vec![
                vec![layout_box("left", 60.0, 290.0)],
                vec![layout_box("right", 310.0, 540.0)],
            ],
            bands: vec![false, false],
            boundaries: vec![300.0],
        };
        assert!(!is_sparse_ruled_split(&two_column, &segments));
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

    /// A minimal multi-page PDF, one text line per (y, text) entry.
    /// Generated in the test so the geometry it depends on is visible
    /// here rather than hidden in a committed binary.
    fn multi_page_pdf(pages: &[Vec<(u32, &str)>]) -> Vec<u8> {
        let mut out = String::from("%PDF-1.4\n");
        let kids: Vec<String> = (0..pages.len())
            .map(|i| format!("{} 0 R", 4 + i * 2))
            .collect();
        out.push_str("1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n");
        out.push_str(&format!(
            "2 0 obj << /Type /Pages /Kids [{}] /Count {} >> endobj\n",
            kids.join(" "),
            pages.len()
        ));
        out.push_str("3 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n");
        for (i, lines) in pages.iter().enumerate() {
            let page_obj = 4 + i * 2;
            let content_obj = page_obj + 1;
            out.push_str(&format!(
                "{page_obj} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 3 0 R >> >> /Contents {content_obj} 0 R >> endobj\n"
            ));
            let mut stream = String::new();
            for (y, text) in lines {
                stream.push_str(&format!("BT /F1 12 Tf 72 {y} Td ({text}) Tj ET\n"));
            }
            out.push_str(&format!(
                "{content_obj} 0 obj << /Length {} >> stream\n{stream}endstream endobj\n",
                stream.len()
            ));
        }
        out.push_str("trailer << /Root 1 0 R >>");
        out.into_bytes()
    }

    fn convert_bytes(input: &[u8], page_markers: bool) -> String {
        PdfConverter
            .convert(
                input,
                &StreamInfo {
                    extension: Some(".pdf".into()),
                    page_markers,
                    ..Default::default()
                },
            )
            .expect("conversion")
            .markdown
    }

    /// Markers are additive: take them out and what is left is exactly
    /// the default run's markdown.
    fn without_markers(markdown: &str) -> String {
        markdown
            .lines()
            .filter(|line| !line.starts_with(PAGE_MARKER_PREFIX))
            .collect::<Vec<_>>()
            .join("\n")
            .split("\n\n")
            .map(str::trim)
            .filter(|block| !block.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Three pages, the middle one blank: a page with no content still
    /// holds a page number, so it still gets a marker.
    fn three_page_pdf() -> Vec<u8> {
        multi_page_pdf(&[
            vec![(700, "First page prose.")],
            vec![],
            vec![(700, "Third page prose.")],
        ])
    }

    #[test]
    fn page_markers_are_off_by_default() {
        let markdown = convert_bytes(&three_page_pdf(), false);
        assert!(!markdown.contains(PAGE_MARKER_PREFIX), "{markdown}");
        assert!(markdown.contains("First page prose."), "{markdown}");
        assert_eq!(without_markers(&markdown), markdown);
    }

    #[test]
    fn page_markers_number_every_physical_page() {
        let pdf = three_page_pdf();
        let marked = convert_bytes(&pdf, true);
        let markers: Vec<&str> = marked
            .lines()
            .filter(|line| line.starts_with(PAGE_MARKER_PREFIX))
            .collect();
        assert_eq!(
            markers,
            [
                "<!-- markit:page 1 -->",
                "<!-- markit:page 2 -->",
                "<!-- markit:page 3 -->"
            ],
            "{marked}"
        );
        assert_eq!(without_markers(&marked), convert_bytes(&pdf, false));
    }

    fn convert_fixture(path: &str) -> String {
        let buf = std::fs::read(path).unwrap();
        PdfConverter
            .convert(
                &buf,
                &StreamInfo {
                    extension: Some(".pdf".into()),
                    local_path: Some(path.into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .markdown
    }

    fn normalized_words(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn narrow_table_group_does_not_collapse_page_columns() {
        let path = "testdata/pdf/regression/00f6c2eea6b51fb1bf637b5f8a850588d7e2_page_10_pg1.pdf";
        let markdown = convert_fixture(path);
        assert!(
            normalized_words(&markdown).contains(
                "The proposed method is implemented using the Python programming language."
            ),
            "column prose was interleaved:\n{markdown}"
        );
    }

    #[test]
    fn prose_gutter_beats_tabular_row_fragments() {
        let path = "testdata/pdf/regression/013a3686ef75b4150c07d3ebc510ca2455d6_page_1_pg1.pdf";
        let markdown = convert_fixture(path);
        assert!(
            normalized_words(&markdown)
                .contains("This sudden surge of international students is attributed"),
            "two-column prose was merged row-wise:\n{markdown}"
        );
    }

    #[test]
    fn full_pipeline_with_fixture() {
        let fixture = "testdata/pdf/regression/09f90a8fad0997f7cf454cbcbe79cab3bc0f_page_1_pg1.pdf";
        assert!(Path::new(fixture).exists(), "committed fixture missing");

        let markdown = convert_fixture(fixture);

        assert!(!markdown.is_empty());
        assert!(
            markdown.contains("Open-Domain Textual Question Answering"),
            "Output should contain key text from the PDF"
        );
    }
}
