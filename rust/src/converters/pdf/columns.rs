//! Multi-column layout detection and text box reordering.
//!
//! Many PDFs (legal documents, datasheets, academic papers) use two-column
//! layouts. Without column detection, text boxes are ordered by Y position
//! only, interleaving left and right column content.
//!
//! Algorithm:
//!   1. Collect left edges of all text boxes on the page
//!   2. Find the largest horizontal gap between consecutive left edges
//!   3. If gap > MIN_GAP_RATIO of the text width and both sides have
//!      enough boxes → multi-column detected
//!   4. Assign each text box to a column based on its center X
//!   5. Return columns in reading order (left-to-right, top-to-bottom)
//!
//! This only detects the column structure. The caller is responsible for
//! processing each column's text boxes independently (table detection,
//! rendering, etc.).

use std::collections::BTreeSet;

use crate::converters::pdf::types::TextBox;

/// Minimum gap as a fraction of the total text width to consider a column
/// boundary. A two-column layout typically has ~50% gap; we use a lower
/// threshold to catch asymmetric columns.
const MIN_GAP_RATIO: f64 = 0.15;

/// Minimum number of text boxes on each side of the gap.
const MIN_BOXES_PER_COLUMN: usize = 4;

/// Minimum gap in absolute points to avoid splitting on small whitespace.
const MIN_GAP_PTS: f64 = 40.0;

/// Result of column layout detection.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnLayout {
    /// Number of columns detected (1 = single column, 2+ = multi-column).
    pub column_count: usize,
    /// Text boxes grouped by column, in reading order (left to right).
    pub columns: Vec<Vec<TextBox>>,
    /// X positions of column boundaries (between columns).
    pub boundaries: Vec<f64>,
}

/// Detect column layout and return text boxes grouped by column.
///
/// For single-column pages, returns all boxes in one group.
/// For multi-column pages, returns boxes split by column in reading order.
pub fn detect_columns(text_boxes: &[TextBox]) -> ColumnLayout {
    if text_boxes.len() < MIN_BOXES_PER_COLUMN * 2 {
        return ColumnLayout {
            column_count: 1,
            columns: vec![text_boxes.to_vec()],
            boundaries: vec![],
        };
    }

    // Collect unique left edges (rounded to avoid float noise)
    let lefts_set: BTreeSet<i64> = text_boxes
        .iter()
        .map(|tb| tb.bounds.left.round() as i64)
        .collect();
    let lefts: Vec<i64> = lefts_set.into_iter().collect(); // already sorted by BTreeSet

    if lefts.len() < 2 {
        return ColumnLayout {
            column_count: 1,
            columns: vec![text_boxes.to_vec()],
            boundaries: vec![],
        };
    }

    let text_x_min = lefts[0] as f64;
    let text_x_max = text_boxes
        .iter()
        .map(|tb| tb.bounds.right.round() as i64)
        .max()
        .unwrap_or(0) as f64;
    let text_width = text_x_max - text_x_min;

    if text_width <= 0.0 {
        return ColumnLayout {
            column_count: 1,
            columns: vec![text_boxes.to_vec()],
            boundaries: vec![],
        };
    }

    // Find the largest gap between consecutive left-edge positions
    let mut max_gap: i64 = 0;
    let mut gap_left: i64 = 0;
    let mut gap_right: i64 = 0;

    for i in 1..lefts.len() {
        let gap = lefts[i] - lefts[i - 1];
        if gap > max_gap {
            max_gap = gap;
            gap_left = lefts[i - 1];
            gap_right = lefts[i];
        }
    }

    let gap_ratio = max_gap as f64 / text_width;

    if gap_ratio < MIN_GAP_RATIO || (max_gap as f64) < MIN_GAP_PTS {
        return ColumnLayout {
            column_count: 1,
            columns: vec![text_boxes.to_vec()],
            boundaries: vec![],
        };
    }

    // Split point is the midpoint of the gap
    let split_x = (gap_left as f64 + gap_right as f64) / 2.0;

    // Assign boxes to columns based on center X
    let mut left_col: Vec<TextBox> = Vec::new();
    let mut right_col: Vec<TextBox> = Vec::new();

    for tb in text_boxes {
        let cx = (tb.bounds.left + tb.bounds.right) / 2.0;
        if cx < split_x {
            left_col.push(tb.clone());
        } else {
            right_col.push(tb.clone());
        }
    }

    // Validate both columns have enough content
    if left_col.len() < MIN_BOXES_PER_COLUMN || right_col.len() < MIN_BOXES_PER_COLUMN {
        return ColumnLayout {
            column_count: 1,
            columns: vec![text_boxes.to_vec()],
            boundaries: vec![],
        };
    }

    ColumnLayout {
        column_count: 2,
        columns: vec![left_col, right_col],
        boundaries: vec![split_x],
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
}
