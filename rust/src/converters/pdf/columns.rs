//! Multi-column layout detection and text box reordering.
//!
//! Many PDFs (legal documents, datasheets, academic papers) use two-column
//! layouts. Without column detection, text boxes are ordered by Y position
//! only, interleaving left and right column content.
//!
//! Algorithm (article-level, coverage-based):
//!   1. Build an x-coverage histogram: for each 1pt bin, count the boxes
//!      whose horizontal interval strictly crosses it.
//!   2. A gutter is a run of bins that almost no box crosses (full-width
//!      titles and headings are allowed to cross), wide enough, with
//!      enough boxes fully on each side.
//!   3. Boxes crossing a gutter are full-width "bands" (titles, section
//!      headings, footers). The rest are column-bound.
//!   4. Walk the page top-to-bottom: bands split the page into vertical
//!      regions; each region's boxes are emitted column by column
//!      (left to right), preserving article reading order.
//!
//! This only detects the structure. The caller is responsible for
//! processing each group's text boxes independently (table detection,
//! rendering, etc.).

use crate::converters::pdf::types::TextBox;

/// Minimum number of text boxes fully on each side of a gutter.
const MIN_BOXES_PER_COLUMN: usize = 4;

/// Minimum gutter width in points.
const MIN_GUTTER_PTS: i64 = 12;

/// Fraction of the text width excluded at each edge when searching for
/// gutters — a gutter in the outer margins is ragged-edge whitespace,
/// not a column separator.
const GUTTER_SEARCH_MARGIN: f64 = 0.15;

/// Fraction of the page's boxes allowed to cross a gutter (full-width
/// titles, headings, footnote rules). More crossings than this means the
/// whitespace is coincidental, not structural.
const MAX_CROSSING_FRACTION: f64 = 0.15;

/// Maximum number of gutters (three-column layouts).
const MAX_GUTTERS: usize = 2;

/// Result of column layout detection.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnLayout {
    /// Number of groups in reading order (1 = single column).
    pub column_count: usize,
    /// Text boxes grouped in reading order.
    pub columns: Vec<Vec<TextBox>>,
    /// True for groups that are full-width bands (titles, headings).
    pub bands: Vec<bool>,
    /// X positions of column gutter centers.
    pub boundaries: Vec<f64>,
}

fn single(text_boxes: &[TextBox]) -> ColumnLayout {
    ColumnLayout {
        column_count: 1,
        columns: vec![text_boxes.to_vec()],
        bands: vec![false],
        boundaries: vec![],
    }
}

/// Gutter centers found via the crossing histogram, best-first capped.
fn find_gutters(text_boxes: &[TextBox]) -> Vec<f64> {
    let x_min = text_boxes
        .iter()
        .map(|tb| tb.bounds.left)
        .fold(f64::INFINITY, f64::min);
    let x_max = text_boxes
        .iter()
        .map(|tb| tb.bounds.right)
        .fold(f64::NEG_INFINITY, f64::max);
    let width = x_max - x_min;
    if width <= 0.0 {
        return vec![];
    }

    let lo = (x_min + width * GUTTER_SEARCH_MARGIN).ceil() as i64;
    let hi = (x_min + width * (1.0 - GUTTER_SEARCH_MARGIN)).floor() as i64;
    if hi <= lo {
        return vec![];
    }

    let max_crossing = 1.max((text_boxes.len() as f64 * MAX_CROSSING_FRACTION) as usize);

    // Runs of bins crossed by at most max_crossing boxes.
    let mut runs: Vec<(i64, i64)> = Vec::new();
    let mut run_start: Option<i64> = None;
    for x in lo..=hi {
        let crossing = text_boxes
            .iter()
            .filter(|tb| tb.bounds.left + 2.0 < x as f64 && (x as f64) < tb.bounds.right - 2.0)
            .count();
        if crossing <= max_crossing {
            if run_start.is_none() {
                run_start = Some(x);
            }
        } else if let Some(start) = run_start.take() {
            runs.push((start, x - 1));
        }
    }
    if let Some(start) = run_start {
        runs.push((start, hi));
    }

    // Validate: wide enough, and enough boxes fully on each side.
    let mut centers: Vec<(f64, i64)> = Vec::new();
    for (start, end) in runs {
        let run_width = end - start + 1;
        if run_width < MIN_GUTTER_PTS {
            continue;
        }
        let center = (start + end) as f64 / 2.0;
        let left_count = text_boxes
            .iter()
            .filter(|tb| tb.bounds.right <= center)
            .count();
        let right_count = text_boxes
            .iter()
            .filter(|tb| tb.bounds.left >= center)
            .count();
        if left_count < MIN_BOXES_PER_COLUMN || right_count < MIN_BOXES_PER_COLUMN {
            continue;
        }
        centers.push((center, run_width));
    }

    centers.sort_by_key(|c| std::cmp::Reverse(c.1));
    centers.truncate(MAX_GUTTERS);
    let mut gutters: Vec<f64> = centers.into_iter().map(|(c, _)| c).collect();
    gutters.sort_by(|a, b| a.total_cmp(b));
    gutters
}

/// Detect column layout and return text boxes grouped in reading order.
///
/// For single-column pages, returns all boxes in one group. For
/// multi-column pages, returns full-width bands and per-region columns
/// as separate groups in article reading order.
pub fn detect_columns(text_boxes: &[TextBox]) -> ColumnLayout {
    if text_boxes.len() < MIN_BOXES_PER_COLUMN * 2 {
        return single(text_boxes);
    }

    let gutters = find_gutters(text_boxes);
    if gutters.is_empty() {
        return single(text_boxes);
    }

    let crosses_gutter = |tb: &TextBox| -> bool {
        gutters
            .iter()
            .any(|g| tb.bounds.left + 2.0 < *g && *g < tb.bounds.right - 2.0)
    };
    let column_of = |tb: &TextBox| -> usize {
        let center_x = (tb.bounds.left + tb.bounds.right) / 2.0;
        gutters
            .iter()
            .position(|g| center_x < *g)
            .unwrap_or(gutters.len())
    };

    // Walk top-to-bottom (Y-up: larger top first). Bands flush the open
    // region; consecutive band boxes group together.
    let mut ordered = text_boxes.to_vec();
    ordered.sort_by(|a, b| b.bounds.top.total_cmp(&a.bounds.top));

    let mut groups: Vec<Vec<TextBox>> = Vec::new();
    let mut bands: Vec<bool> = Vec::new();
    let mut region_columns: Vec<Vec<TextBox>> = vec![Vec::new(); gutters.len() + 1];
    let mut open_band: Vec<TextBox> = Vec::new();

    for tb in ordered {
        if crosses_gutter(&tb) {
            // Flush the open region.
            for column in region_columns.iter_mut() {
                if !column.is_empty() {
                    groups.push(std::mem::take(column));
                    bands.push(false);
                }
            }
            open_band.push(tb);
        } else {
            // Flush the open band.
            if !open_band.is_empty() {
                groups.push(std::mem::take(&mut open_band));
                bands.push(true);
            }
            region_columns[column_of(&tb)].push(tb);
        }
    }
    for column in region_columns.iter_mut() {
        if !column.is_empty() {
            groups.push(std::mem::take(column));
            bands.push(false);
        }
    }
    if !open_band.is_empty() {
        groups.push(open_band);
        bands.push(true);
    }

    if groups.len() <= 1 {
        return single(text_boxes);
    }

    ColumnLayout {
        column_count: groups.len(),
        columns: groups,
        bands,
        boundaries: gutters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converters::pdf::types::Bounds;
    use std::sync::atomic::{AtomicU64, Ordering};

    static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tb(text: &str, x: f64, y: f64, w: f64) -> TextBox {
        let id = ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        TextBox {
            id: format!("t{id}"),
            text: text.to_string(),
            page_number: 1,
            font_size: 9.0,
            is_bold: false,
            bounds: Bounds {
                left: x,
                right: x + w,
                bottom: y,
                top: y + 10.0,
            },
        }
    }

    fn tb_default(text: &str, x: f64, y: f64) -> TextBox {
        tb(text, x, y, 200.0)
    }

    #[test]
    fn returns_1_column_for_too_few_boxes() {
        let boxes = vec![tb_default("A", 100.0, 500.0), tb_default("B", 100.0, 480.0)];
        let result = detect_columns(&boxes);
        assert_eq!(result.column_count, 1);
        assert_eq!(result.columns.len(), 1);
    }

    #[test]
    fn returns_1_column_for_single_column_layout() {
        let boxes: Vec<TextBox> = (0..20)
            .map(|i| tb_default(&format!("Line {i}"), 72.0, 700.0 - i as f64 * 15.0))
            .collect();
        let result = detect_columns(&boxes);
        assert_eq!(result.column_count, 1);
    }

    #[test]
    fn detects_two_column_layout() {
        // Left column at x=72, right column at x=315 (like the US Constitution)
        let left: Vec<TextBox> = (0..10)
            .map(|i| tb_default(&format!("Left {i}"), 72.0, 700.0 - i as f64 * 15.0))
            .collect();
        let right: Vec<TextBox> = (0..10)
            .map(|i| tb_default(&format!("Right {i}"), 315.0, 700.0 - i as f64 * 15.0))
            .collect();
        let combined: Vec<TextBox> = left.into_iter().chain(right).collect();
        let result = detect_columns(&combined);
        assert_eq!(result.column_count, 2);
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.boundaries.len(), 1);
    }

    #[test]
    fn detects_three_column_layout() {
        let boxes: Vec<TextBox> = [0.0, 200.0, 400.0]
            .into_iter()
            .enumerate()
            .flat_map(|(column, x)| {
                (0..6)
                    .map(move |i| tb(&format!("C{column}-{i}"), x, 700.0 - i as f64 * 15.0, 100.0))
            })
            .collect();
        let result = detect_columns(&boxes);
        assert_eq!(result.column_count, 3);
        assert_eq!(result.columns.len(), 3);
        assert_eq!(result.boundaries.len(), 2);
    }

    #[test]
    fn left_column_comes_first_in_reading_order() {
        let left: Vec<TextBox> = (0..10)
            .map(|i| tb_default(&format!("L{i}"), 72.0, 700.0 - i as f64 * 15.0))
            .collect();
        let right: Vec<TextBox> = (0..10)
            .map(|i| tb_default(&format!("R{i}"), 315.0, 700.0 - i as f64 * 15.0))
            .collect();
        // shuffled input: right first, then left
        let combined: Vec<TextBox> = right.into_iter().chain(left).collect();
        let result = detect_columns(&combined);
        assert!(result.columns[0].iter().all(|b| b.text.starts_with('L')));
        assert!(result.columns[1].iter().all(|b| b.text.starts_with('R')));
    }

    #[test]
    fn does_not_split_when_gap_is_too_small() {
        // Two groups with a small gap — indented text, not real columns
        // Left at x=72 (w=200, right=272), "right" at x=100 (w=200, right=300)
        // Gap between left edges: 100-72=28pt, textWidth=300-72=228, ratio=0.12 < 0.15
        let left: Vec<TextBox> = (0..10)
            .map(|i| tb_default(&format!("A{i}"), 72.0, 700.0 - i as f64 * 15.0))
            .collect();
        let right: Vec<TextBox> = (0..10)
            .map(|i| tb_default(&format!("B{i}"), 100.0, 700.0 - i as f64 * 15.0))
            .collect();
        let combined: Vec<TextBox> = left.into_iter().chain(right).collect();
        let result = detect_columns(&combined);
        assert_eq!(result.column_count, 1);
    }

    #[test]
    fn does_not_split_when_one_side_has_too_few_boxes() {
        let left: Vec<TextBox> = (0..15)
            .map(|i| tb_default(&format!("Main {i}"), 72.0, 700.0 - i as f64 * 15.0))
            .collect();
        let right = vec![tb_default("Margin note", 400.0, 600.0)]; // only 1 box on right
        let combined: Vec<TextBox> = left.into_iter().chain(right).collect();
        let result = detect_columns(&combined);
        assert_eq!(result.column_count, 1);
    }

    #[test]
    fn keeps_full_width_title_as_band_above_two_columns() {
        // Title spans both columns; body is two columns below it. The old
        // left-edge heuristic collapsed this page to one row-wise column.
        let title = tb("A Full Width Paper Title", 100.0, 760.0, 350.0);
        let authors = tb("A. Author and B. Author", 150.0, 740.0, 250.0);
        let left: Vec<TextBox> = (0..8)
            .map(|i| tb_default(&format!("L{i}"), 72.0, 700.0 - i as f64 * 15.0))
            .collect();
        let right: Vec<TextBox> = (0..8)
            .map(|i| tb_default(&format!("R{i}"), 315.0, 700.0 - i as f64 * 15.0))
            .collect();
        let combined: Vec<TextBox> = [title, authors]
            .into_iter()
            .chain(left)
            .chain(right)
            .collect();
        let result = detect_columns(&combined);
        assert_eq!(result.column_count, 3);
        assert_eq!(result.bands, [true, false, false]);
        let texts: Vec<&str> = result.columns[0].iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            ["A Full Width Paper Title", "A. Author and B. Author"]
        );
        assert!(result.columns[1].iter().all(|b| b.text.starts_with('L')));
        assert!(result.columns[2].iter().all(|b| b.text.starts_with('R')));
    }

    #[test]
    fn splits_regions_at_mid_page_full_width_heading() {
        let upper_left: Vec<TextBox> = (0..5)
            .map(|i| tb_default(&format!("UL{i}"), 72.0, 700.0 - i as f64 * 15.0))
            .collect();
        let upper_right: Vec<TextBox> = (0..5)
            .map(|i| tb_default(&format!("UR{i}"), 315.0, 700.0 - i as f64 * 15.0))
            .collect();
        let heading = tb(
            "A Section Heading Spanning Both Columns",
            90.0,
            600.0,
            380.0,
        );
        let lower_left: Vec<TextBox> = (0..5)
            .map(|i| tb_default(&format!("LL{i}"), 72.0, 560.0 - i as f64 * 15.0))
            .collect();
        let lower_right: Vec<TextBox> = (0..5)
            .map(|i| tb_default(&format!("LR{i}"), 315.0, 560.0 - i as f64 * 15.0))
            .collect();
        let combined: Vec<TextBox> = upper_left
            .into_iter()
            .chain(upper_right)
            .chain([heading])
            .chain(lower_left)
            .chain(lower_right)
            .collect();
        let result = detect_columns(&combined);
        assert_eq!(result.bands, [false, false, true, false, false]);
        let firsts: Vec<&str> = result.columns.iter().map(|g| g[0].text.as_str()).collect();
        assert_eq!(
            firsts,
            [
                "UL0",
                "UR0",
                "A Section Heading Spanning Both Columns",
                "LL0",
                "LR0"
            ]
        );
    }
}
