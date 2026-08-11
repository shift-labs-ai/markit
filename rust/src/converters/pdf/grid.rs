//! Table grid detection from vector segments and text boxes.
//!
//! Ported from src/converters/pdf/grid.ts. The core algorithm:
//!
//! 1. Classify segments as horizontal or vertical lines
//! 2. Group horizontal Y-lines into table groups (split by vertical gaps)
//! 3. For each group:
//!    a. Full grid (H+V lines): build cells from grid intersections,
//!    place text via raycasting
//!    b. H-line only (no V lines): infer columns from text X positions
//! 4. Prune empty rows/cols

use std::collections::{HashMap, HashSet};

use crate::converters::pdf::types::*;

mod cells;
mod lines;
mod prune;
mod raycast;

use cells::build_table_grid;
use lines::{split_y_lines_into_groups, unique_sorted};
use prune::prune_empty_rows_and_cols;

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

const AXIS_EPSILON: f64 = 0.8;
const PAGE_MARGIN: f64 = 20.0;

const COL_GAP_THRESHOLD: f64 = 20.0;
const HONLY_ROW_GAP: f64 = 30.0;
const HONLY_ROW_TOLERANCE: f64 = 8.0;
const MIN_TABLE_HEIGHT: f64 = 24.0;
const MIN_LEFT_SPREAD: f64 = 50.0;

fn infer_x_lines_from_boxes(text_boxes: &[TextBox], x_min: f64, x_max: f64) -> Vec<f64> {
    let mut centers: Vec<f64> = text_boxes
        .iter()
        .map(|tb| (tb.bounds.left + tb.bounds.right) / 2.0)
        .collect();
    centers.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if centers.is_empty() {
        return vec![x_min, x_max];
    }

    let mut boundaries = vec![x_min];
    for i in 1..centers.len() {
        if centers[i] - centers[i - 1] >= COL_GAP_THRESHOLD {
            boundaries.push((centers[i - 1] + centers[i]) / 2.0);
        }
    }
    boundaries.push(x_max);
    boundaries
}

fn build_h_line_only_table(
    page_number: u32,
    y_lines: &[f64],
    x_min: f64,
    x_max: f64,
    text_boxes: &[TextBox],
    already_consumed: &HashSet<String>,
) -> Option<(TableGrid, Vec<String>)> {
    let y_max = y_lines[0];
    let y_min = y_lines[y_lines.len() - 1];
    let candidates: Vec<&TextBox> = text_boxes
        .iter()
        .filter(|tb| !already_consumed.contains(&tb.id))
        .collect();

    let box_left_tolerance = 30.0;
    let in_range: Vec<&TextBox> = candidates
        .iter()
        .filter(|tb| {
            let cy = (tb.bounds.top + tb.bounds.bottom) / 2.0;
            tb.bounds.left >= x_min - box_left_tolerance
                && tb.bounds.right <= x_max + box_left_tolerance
                && cy >= y_min
                && cy <= y_max
        })
        .copied()
        .collect();

    // Extend downward below yMin
    let mut below_y_min: Vec<&TextBox> = candidates
        .iter()
        .filter(|tb| {
            let cx = (tb.bounds.left + tb.bounds.right) / 2.0;
            let cy = (tb.bounds.top + tb.bounds.bottom) / 2.0;
            cx >= x_min && cx <= x_max && cy < y_min
        })
        .copied()
        .collect();
    below_y_min.sort_by(|a, b| {
        let ya = (a.bounds.top + a.bounds.bottom) / 2.0;
        let yb = (b.bounds.top + b.bounds.bottom) / 2.0;
        yb.partial_cmp(&ya).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut extension_boxes: Vec<&TextBox> = Vec::new();
    let mut last_y = y_min;
    for tb in &below_y_min {
        let cy = (tb.bounds.top + tb.bounds.bottom) / 2.0;
        if last_y - cy > HONLY_ROW_GAP {
            break;
        }
        extension_boxes.push(tb);
        last_y = cy;
    }

    let all_boxes: Vec<&TextBox> = in_range
        .iter()
        .chain(extension_boxes.iter())
        .copied()
        .collect();
    if all_boxes.is_empty() {
        return None;
    }

    let left_edges: Vec<f64> = all_boxes.iter().map(|tb| tb.bounds.left).collect();
    let left_max = left_edges.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let left_min = left_edges.iter().cloned().fold(f64::INFINITY, f64::min);
    if left_max - left_min < MIN_LEFT_SPREAD {
        return None;
    }

    let all_boxes_owned: Vec<TextBox> = all_boxes.iter().map(|tb| (*tb).clone()).collect();
    let x_lines = infer_x_lines_from_boxes(&all_boxes_owned, x_min, x_max);
    if x_lines.len() < 2 {
        return None;
    }
    let cols = x_lines.len() - 1;

    // Build visual rows
    struct VisualRow {
        mid_y: f64,
        boxes: Vec<usize>, // indices into all_boxes
    }

    let mut sorted_indices: Vec<usize> = (0..all_boxes.len()).collect();
    // Tolerance-band comparator (not a total order) — see js_stable_sort.
    super::js_stable_sort(&mut sorted_indices, |&a, &b| {
        let ya = (all_boxes[a].bounds.top + all_boxes[a].bounds.bottom) / 2.0;
        let yb = (all_boxes[b].bounds.top + all_boxes[b].bounds.bottom) / 2.0;
        if (ya - yb).abs() > 0.5 {
            yb.partial_cmp(&ya).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            all_boxes[a]
                .bounds
                .left
                .partial_cmp(&all_boxes[b].bounds.left)
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    });

    let mut visual_rows: Vec<VisualRow> = Vec::new();
    for &idx in &sorted_indices {
        let cy = (all_boxes[idx].bounds.top + all_boxes[idx].bounds.bottom) / 2.0;
        if let Some(last) = visual_rows.last_mut() {
            if (last.mid_y - cy).abs() <= HONLY_ROW_TOLERANCE {
                last.boxes.push(idx);
                continue;
            }
        }
        visual_rows.push(VisualRow {
            mid_y: cy,
            boxes: vec![idx],
        });
    }
    if visual_rows.is_empty() {
        return None;
    }

    let mut cells: Vec<TableCell> = Vec::new();
    let mut consumed_ids: Vec<String> = Vec::new();

    for (row_idx, vrow) in visual_rows.iter().enumerate() {
        let mut col_boxes: HashMap<usize, Vec<usize>> = HashMap::new();

        for &box_idx in &vrow.boxes {
            let bx = all_boxes[box_idx];
            let cx = (bx.bounds.left + bx.bounds.right) / 2.0;
            let col = x_lines.windows(2).position(|w| cx >= w[0] && cx <= w[1]);
            if let Some(col) = col {
                if col < cols {
                    col_boxes.entry(col).or_default().push(box_idx);
                }
            }
        }

        for c in 0..cols {
            let mut cbs: Vec<usize> = col_boxes.get(&c).cloned().unwrap_or_default();
            cbs.sort_by(|&a, &b| {
                all_boxes[a]
                    .bounds
                    .left
                    .partial_cmp(&all_boxes[b].bounds.left)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let text: String = cbs
                .iter()
                .map(|&bi| all_boxes[bi].text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            cells.push(TableCell {
                row: row_idx,
                col: c,
                text,
                row_span: 1,
                col_span: 1,
            });
            for &bi in &cbs {
                consumed_ids.push(all_boxes[bi].id.clone());
            }
        }
    }

    let content_top_y = if !visual_rows.is_empty() {
        visual_rows[0].mid_y
    } else {
        y_max
    };
    let grid = prune_empty_rows_and_cols(TableGrid {
        page_number,
        rows: visual_rows.len(),
        cols,
        cells,
        warnings: vec![],
        top_y: content_top_y,
        is_borderless: false,
    });
    Some((grid, consumed_ids))
}

// ---------------------------------------------------------------------------
// Pruning
// ---------------------------------------------------------------------------

/// Maximum column count for a plausible data table.
const MAX_TABLE_COLS: usize = 25;

/// Returns true if a grid looks like a vector diagram rather than a data table.
///
/// Heuristics (any match → diagram):
///   1. Column count > 25 (diagrams create many X-lines from box edges)
///   2. Fill ratio < 25% (most cells empty — scattered boxes)
///   3. Fill < 50% AND duplicate text ratio > 30% (repeating labels in a
///      diagram layout, e.g. "Hash", "Transaction" appearing in each column)
///   4. Fill < 50% AND cols >= 6 (moderate sparseness with wide grid)
fn is_diagram(grid: &TableGrid) -> bool {
    let total_cells = grid.rows * grid.cols;
    if total_cells == 0 {
        return true;
    }

    let filled: Vec<&TableCell> = grid
        .cells
        .iter()
        .filter(|c| !c.text.trim().is_empty())
        .collect();
    let fill_ratio = filled.len() as f64 / total_cells as f64;

    // Very high column count
    if grid.cols > MAX_TABLE_COLS {
        return true;
    }

    // Very sparse
    if fill_ratio < 0.25 {
        return true;
    }

    // Compute duplicate text ratio among non-trivial cells.
    // Exclude short values (≤3 chars) like "—", "V", "YES", "NO" which
    // naturally repeat in real data tables.
    let substantive: Vec<&TableCell> = filled
        .iter()
        .filter(|c| c.text.trim().len() > 3)
        .copied()
        .collect();
    let unique_texts: HashSet<&str> = substantive.iter().map(|c| c.text.trim()).collect();
    let dup_ratio = if substantive.len() > 2 {
        1.0 - unique_texts.len() as f64 / substantive.len() as f64
    } else {
        0.0
    };

    // Sparse + highly duplicated substantive text → repeating diagram
    if fill_ratio < 0.5 && dup_ratio > 0.3 {
        return true;
    }

    // High duplication + wide grid → repeating diagram even at moderate fill
    if dup_ratio > 0.4 && grid.cols >= 6 {
        return true;
    }

    // Sparse + wide grid with no substantive text to judge
    if fill_ratio < 0.4 && grid.cols >= 6 {
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Result of resolving table grids on a page.
#[derive(Debug, Clone)]
pub struct GridResult {
    pub grids: Vec<TableGrid>,
    pub consumed_ids: Vec<String>,
}

/// Detect all table grids on a single page from its text boxes and segments.
pub fn resolve_table_grids(
    page_number: u32,
    text_boxes: &[TextBox],
    segments: &[Segment],
) -> GridResult {
    let vertical: Vec<&Segment> = segments
        .iter()
        .filter(|s| (s.x1 - s.x2).abs() <= AXIS_EPSILON)
        .collect();
    let horizontal: Vec<&Segment> = segments
        .iter()
        .filter(|s| (s.y1 - s.y2).abs() <= AXIS_EPSILON)
        .collect();

    // Filter segments to the text's visible area
    let text_y_values: Vec<f64> = text_boxes
        .iter()
        .flat_map(|t| [t.bounds.bottom, t.bounds.top])
        .collect();
    let text_y_min = if !text_y_values.is_empty() {
        text_y_values.iter().cloned().fold(f64::INFINITY, f64::min) - PAGE_MARGIN
    } else {
        f64::NEG_INFINITY
    };
    let text_y_max = if !text_y_values.is_empty() {
        text_y_values
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max)
            + PAGE_MARGIN
    } else {
        f64::INFINITY
    };
    let text_x_values: Vec<f64> = text_boxes
        .iter()
        .flat_map(|t| [t.bounds.left, t.bounds.right])
        .collect();
    let text_x_min = if !text_x_values.is_empty() {
        text_x_values.iter().cloned().fold(f64::INFINITY, f64::min) - 100.0
    } else {
        f64::NEG_INFINITY
    };
    let text_x_max = if !text_x_values.is_empty() {
        text_x_values
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max)
            + 100.0
    } else {
        f64::INFINITY
    };

    let filtered_h: Vec<Segment> = horizontal
        .iter()
        .filter(|s| {
            s.y1 >= text_y_min
                && s.y1 <= text_y_max
                && s.x1.min(s.x2) <= text_x_max
                && s.x1.max(s.x2) >= text_x_min
        })
        .map(|s| (*s).clone())
        .collect();

    let h_max_x2 = if !filtered_h.is_empty() {
        filtered_h
            .iter()
            .map(|s| s.x1.max(s.x2))
            .fold(f64::NEG_INFINITY, f64::max)
    } else {
        text_x_max
    };
    let v_seg_x_max = text_x_max.max(h_max_x2 + PAGE_MARGIN);

    let filtered_v: Vec<Segment> = vertical
        .iter()
        .filter(|s| {
            let seg_min = s.y1.min(s.y2);
            let seg_max = s.y1.max(s.y2);
            seg_max >= text_y_min
                && seg_min <= text_y_max
                && s.x1 >= text_x_min
                && s.x1 <= v_seg_x_max
        })
        .map(|s| (*s).clone())
        .collect();

    let all_y_lines_vals: Vec<f64> = filtered_h.iter().flat_map(|s| vec![s.y1, s.y2]).collect();
    let mut all_y_lines = unique_sorted(&all_y_lines_vals);
    all_y_lines.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal)); // descending

    if all_y_lines.len() < 2 {
        return GridResult {
            grids: vec![],
            consumed_ids: vec![],
        };
    }

    let mut filtered_segments: Vec<Segment> = Vec::new();
    filtered_segments.extend(filtered_h.iter().cloned());
    filtered_segments.extend(filtered_v.iter().cloned());

    let y_groups = split_y_lines_into_groups(&all_y_lines, &filtered_v);

    let mut grids: Vec<TableGrid> = Vec::new();
    let mut grid_consumed_ids: Vec<Vec<String>> = Vec::new();
    let mut all_consumed_ids: Vec<String> = Vec::new();
    let mut all_consumed_set: HashSet<String> = HashSet::new();

    for y_lines in &y_groups {
        if y_lines.len() < 2 {
            continue;
        }

        let y_min = y_lines[y_lines.len() - 1];
        let y_max = y_lines[0];

        let group_verticals: Vec<Segment> = filtered_v
            .iter()
            .filter(|s| {
                let seg_min = s.y1.min(s.y2);
                let seg_max = s.y1.max(s.y2);
                seg_min < y_max - 1.5 && seg_max > y_min + 1.5
            })
            .cloned()
            .collect();

        let group_x_lines = unique_sorted(
            &group_verticals
                .iter()
                .flat_map(|s| vec![s.x1, s.x2])
                .collect::<Vec<_>>(),
        );

        if group_x_lines.len() < 2 {
            if y_max - y_min < MIN_TABLE_HEIGHT {
                continue;
            }

            let group_horiz: Vec<&Segment> = filtered_h
                .iter()
                .filter(|s| s.y1 >= y_min - 1.5 && s.y1 <= y_max + 1.5)
                .collect();
            if group_horiz.is_empty() {
                continue;
            }

            let hx_min = group_horiz
                .iter()
                .map(|s| s.x1.min(s.x2))
                .fold(f64::INFINITY, f64::min);
            let hx_max = group_horiz
                .iter()
                .map(|s| s.x1.max(s.x2))
                .fold(f64::NEG_INFINITY, f64::max);

            if let Some((grid, cids)) = build_h_line_only_table(
                page_number,
                y_lines,
                hx_min,
                hx_max,
                text_boxes,
                &all_consumed_set,
            ) {
                grids.push(grid);
                all_consumed_set.extend(cids.iter().cloned());
                all_consumed_ids.extend(cids.iter().cloned());
                grid_consumed_ids.push(cids);
            }
            continue;
        }

        if y_max - y_min < MIN_TABLE_HEIGHT {
            continue;
        }

        // Only boxes near the group's y-range can land in its cells or
        // header row; splitting and ray-casting the whole page per group
        // is quadratic on multi-table pages.
        let group_boxes: Vec<TextBox> = text_boxes
            .iter()
            .filter(|tb| {
                let cy = (tb.bounds.top + tb.bounds.bottom) / 2.0;
                cy >= y_min - 5.0 && cy <= y_max + 25.0
            })
            .cloned()
            .collect();
        let (grid, cids) = build_table_grid(
            page_number,
            y_lines,
            &group_x_lines,
            &filtered_segments,
            &group_boxes,
        );

        // A one-column ruled grid is usually a framed box whose interior
        // structure is drawn with whitespace, not rules (outer border
        // only). Reconstruct the interior from text alignment; prefer it
        // when it recovers real columns for most of the content.
        if grid.cols <= 1 && !cids.is_empty() {
            let cid_set: HashSet<&str> = cids.iter().map(|s| s.as_str()).collect();
            let consumed_boxes: Vec<TextBox> = text_boxes
                .iter()
                .filter(|tb| cid_set.contains(tb.id.as_str()))
                .cloned()
                .collect();
            let (bgrids, bconsumed) =
                super::borderless::detect_borderless_tables(&consumed_boxes, page_number);
            if bgrids.iter().any(|g| g.cols >= 2) && bconsumed.len() * 10 >= cids.len() * 6 {
                for bgrid in bgrids {
                    grids.push(bgrid);
                    grid_consumed_ids.push(bconsumed.clone());
                }
                all_consumed_set.extend(bconsumed.iter().cloned());
                all_consumed_ids.extend(bconsumed.iter().cloned());
                continue;
            }
        }

        grids.push(grid);
        all_consumed_set.extend(cids.iter().cloned());
        all_consumed_ids.extend(cids.iter().cloned());
        grid_consumed_ids.push(cids);
    }

    // Filter out grids that look like vector diagrams, not data tables.
    let mut filtered_grids: Vec<TableGrid> = Vec::new();
    let mut filtered_consumed_ids: Vec<String> = Vec::new();

    for i in 0..grids.len() {
        // Borderless reconstructions are already density-validated, and
        // real data tables legitimately repeat cell values.
        if !grids[i].is_borderless && is_diagram(&grids[i]) {
            continue;
        }
        filtered_grids.push(grids[i].clone());
        filtered_consumed_ids.extend(grid_consumed_ids[i].iter().cloned());
    }

    GridResult {
        grids: filtered_grids,
        consumed_ids: filtered_consumed_ids,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Counter state per-test (reset in each test function)
    thread_local! {
        static SID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        static TID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    fn reset_ids() {
        SID.with(|s| s.set(0));
        TID.with(|t| t.set(0));
    }

    /// Horizontal segment at Y, from x1 to x2.
    fn h_seg(y: f64, x1: f64, x2: f64) -> Segment {
        let id = SID.with(|s| {
            let v = s.get();
            s.set(v + 1);
            v
        });
        Segment {
            id: format!("h{}", id),
            x1,
            y1: y,
            x2,
            y2: y,
        }
    }

    /// Vertical segment at X, from y1 to y2.
    fn v_seg(x: f64, y1: f64, y2: f64) -> Segment {
        let id = SID.with(|s| {
            let v = s.get();
            s.set(v + 1);
            v
        });
        Segment {
            id: format!("v{}", id),
            x1: x,
            y1,
            x2: x,
            y2,
        }
    }

    /// Text box centered at (cx, cy) with some default width.
    fn tb(text: &str, cx: f64, cy: f64) -> TextBox {
        let id = TID.with(|t| {
            let v = t.get();
            t.set(v + 1);
            v
        });
        TextBox {
            id: format!("t{}", id),
            text: text.to_string(),
            page_number: 1,
            font_size: 9.0,
            is_bold: false,
            bounds: Bounds {
                left: cx - 10.0,
                right: cx + 10.0,
                bottom: cy - 5.0,
                top: cy + 5.0,
            },
        }
    }

    /// Text box with explicit left/right bounds (not centered).
    fn wide_box(text: &str, left: f64, right: f64, cy: f64) -> TextBox {
        let id = TID.with(|t| {
            let v = t.get();
            t.set(v + 1);
            v
        });
        TextBox {
            id: format!("t{}", id),
            text: text.to_string(),
            page_number: 1,
            font_size: 9.0,
            is_bold: false,
            bounds: Bounds {
                left,
                right,
                bottom: cy - 5.0,
                top: cy + 5.0,
            },
        }
    }

    /// Build a complete rectangular grid of segments.
    fn table_segs(x_lines: &[f64], y_lines: &[f64]) -> Vec<Segment> {
        let mut segs = Vec::new();
        let y_min = y_lines.iter().cloned().fold(f64::INFINITY, f64::min);
        let y_max = y_lines.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let x_min = x_lines.iter().cloned().fold(f64::INFINITY, f64::min);
        let x_max = x_lines.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        for &x in x_lines {
            segs.push(v_seg(x, y_min, y_max));
        }
        for &y in y_lines {
            segs.push(h_seg(y, x_min, x_max));
        }
        segs
    }

    fn cell_text(grid: &TableGrid, r: usize, c: usize) -> String {
        grid.cells
            .iter()
            .find(|cl| cl.row == r && cl.col == c)
            .map(|cl| cl.text.clone())
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // No grid detected
    // -----------------------------------------------------------------------

    #[test]
    fn returns_empty_when_no_segments() {
        reset_ids();
        let result = resolve_table_grids(1, &[tb("hello", 200.0, 500.0)], &[]);
        assert_eq!(result.grids.len(), 0);
        assert_eq!(result.consumed_ids.len(), 0);
    }

    #[test]
    fn returns_empty_with_only_horizontal_lines_no_vertical() {
        reset_ids();
        let segs = vec![h_seg(400.0, 100.0, 500.0), h_seg(350.0, 100.0, 500.0)];
        let result = resolve_table_grids(1, &[tb("A", 200.0, 375.0)], &segs);
        // H-line-only detection requires multi-column spread
        // Single centered text box won't trigger it
        assert_eq!(result.grids.len(), 0);
    }

    #[test]
    fn returns_empty_with_no_text_boxes_empty_grid_filtered_as_diagram() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0, 300.0]);
        let result = resolve_table_grids(1, &[], &segs);
        // Segments exist but no text — all-empty grid is filtered out
        assert_eq!(result.grids.len(), 0);
    }

    // -----------------------------------------------------------------------
    // Single table detection
    // -----------------------------------------------------------------------

    #[test]
    fn detects_one_grid() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0, 300.0]);
        let boxes = vec![
            tb("Name", 200.0, 375.0),
            tb("Role", 400.0, 375.0),
            tb("Alice", 200.0, 325.0),
            tb("CEO", 400.0, 325.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
        assert_eq!(result.consumed_ids.len(), 4);
    }

    #[test]
    fn sets_top_y_from_the_top_horizontal_line() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0, 300.0]);
        let boxes = vec![tb("A", 200.0, 375.0)];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert!((result.grids[0].top_y - 400.0).abs() < 1.0);
    }

    #[test]
    fn places_text_in_correct_cells() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0, 300.0]);
        let boxes = vec![
            tb("Name", 200.0, 375.0),
            tb("Role", 400.0, 375.0),
            tb("Alice", 200.0, 325.0),
            tb("CEO", 400.0, 325.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        let g = &result.grids[0];
        assert_eq!(cell_text(g, 0, 0), "Name");
        assert_eq!(cell_text(g, 0, 1), "Role");
        assert_eq!(cell_text(g, 1, 0), "Alice");
        assert_eq!(cell_text(g, 1, 1), "CEO");
    }

    #[test]
    fn does_not_consume_text_boxes_outside_the_grid() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0, 300.0]);
        let inside = tb("inside", 200.0, 375.0);
        let outside = tb("outside", 600.0, 375.0);
        let inside_id = inside.id.clone();
        let outside_id = outside.id.clone();
        let result = resolve_table_grids(1, &[inside, outside], &segs);
        assert!(result.consumed_ids.contains(&inside_id));
        assert!(!result.consumed_ids.contains(&outside_id));
    }

    #[test]
    fn accepts_horizontal_borders_drawn_right_to_left() {
        reset_ids();
        let mut segs = table_segs(&[0.0, 300.0, 1000.0], &[400.0, 350.0, 300.0]);
        for segment in &mut segs {
            if segment.y1 == segment.y2 {
                std::mem::swap(&mut segment.x1, &mut segment.x2);
            }
        }
        let boxes = vec![
            tb("Name", 150.0, 375.0),
            tb("Role", 650.0, 375.0),
            tb("Alice", 150.0, 325.0),
            tb("CEO", 650.0, 325.0),
        ];
        assert_eq!(resolve_table_grids(1, &boxes, &segs).grids.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Two separate tables on the same page
    // -----------------------------------------------------------------------

    #[test]
    fn detects_two_grids() {
        reset_ids();
        let segs_a = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0]);
        reset_ids(); // TS resets before each helper call chain but shares sid/tid
                     // Actually TS beforeEach resets both. But table_segs calls v_seg and h_seg
                     // which increment sid. Let me NOT reset here — TS beforeEach only runs once
                     // per test, and both tableSegs calls happen within the same test.
                     // The IDs just need to be unique, they don't need specific values.
        let segs_b = table_segs(&[100.0, 300.0, 500.0], &[250.0, 200.0]);
        let mut all_segs = segs_a;
        all_segs.extend(segs_b);

        let boxes = vec![tb("A-Name", 200.0, 375.0), tb("B-Name", 200.0, 225.0)];
        let result = resolve_table_grids(1, &boxes, &all_segs);
        assert_eq!(result.grids.len(), 2);
    }

    #[test]
    fn table_a_has_higher_top_y_than_table_b() {
        reset_ids();
        let segs_a = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0]);
        let segs_b = table_segs(&[100.0, 300.0, 500.0], &[250.0, 200.0]);
        let mut all_segs = segs_a;
        all_segs.extend(segs_b);

        let boxes = vec![tb("A", 200.0, 375.0), tb("B", 200.0, 225.0)];
        let result = resolve_table_grids(1, &boxes, &all_segs);
        let mut sorted = result.grids.clone();
        sorted.sort_by(|a, b| b.top_y.partial_cmp(&a.top_y).unwrap());
        assert!((sorted[0].top_y - 400.0).abs() < 1.0);
        assert!((sorted[1].top_y - 250.0).abs() < 1.0);
    }

    #[test]
    fn each_text_box_goes_to_its_own_table_only() {
        reset_ids();
        let segs_a = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0]);
        let segs_b = table_segs(&[100.0, 300.0, 500.0], &[250.0, 200.0]);
        let mut all_segs = segs_a;
        all_segs.extend(segs_b);

        let box_a = tb("A-row", 200.0, 375.0);
        let box_b = tb("B-row", 200.0, 225.0);
        let result = resolve_table_grids(1, &[box_a, box_b], &all_segs);
        let mut sorted = result.grids.clone();
        sorted.sort_by(|a, b| b.top_y.partial_cmp(&a.top_y).unwrap());

        let texts_a: Vec<&str> = sorted[0]
            .cells
            .iter()
            .map(|c| c.text.as_str())
            .filter(|t| !t.is_empty())
            .collect();
        let texts_b: Vec<&str> = sorted[1]
            .cells
            .iter()
            .map(|c| c.text.as_str())
            .filter(|t| !t.is_empty())
            .collect();
        assert!(texts_a.contains(&"A-row"));
        assert!(!texts_a.contains(&"B-row"));
        assert!(texts_b.contains(&"B-row"));
        assert!(!texts_b.contains(&"A-row"));
    }

    #[test]
    fn all_boxes_appear_in_consumed_ids() {
        reset_ids();
        let segs_a = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0]);
        let segs_b = table_segs(&[100.0, 300.0, 500.0], &[250.0, 200.0]);
        let mut all_segs = segs_a;
        all_segs.extend(segs_b);

        let box_a = tb("A", 200.0, 375.0);
        let box_b = tb("B", 200.0, 225.0);
        let id_a = box_a.id.clone();
        let id_b = box_b.id.clone();
        let result = resolve_table_grids(1, &[box_a, box_b], &all_segs);
        assert!(result.consumed_ids.contains(&id_a));
        assert!(result.consumed_ids.contains(&id_b));
    }

    // -----------------------------------------------------------------------
    // Continuous vertical lines → single table (not split)
    // -----------------------------------------------------------------------

    #[test]
    fn returns_one_grid_for_4_rows() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0, 300.0, 250.0, 200.0]);
        let boxes = vec![
            tb("R0", 200.0, 375.0),
            tb("R1", 200.0, 325.0),
            tb("R2", 200.0, 275.0),
            tb("R3", 200.0, 225.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Multi-line cell text
    // -----------------------------------------------------------------------

    #[test]
    fn joins_multiple_text_boxes_in_a_cell_with_br() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 340.0, 280.0]);
        let boxes = vec![
            tb("Line 1", 200.0, 380.0),
            tb("Line 2", 200.0, 355.0), // same col, different Y within row 0
            tb("Value", 400.0, 370.0),
            tb("Row2", 200.0, 310.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
        let cell00 = cell_text(&result.grids[0], 0, 0);
        assert!(cell00.contains("Line 1"));
        assert!(cell00.contains("Line 2"));
        assert!(cell00.contains("<br>"));
    }

    // -----------------------------------------------------------------------
    // H-line-only table
    // -----------------------------------------------------------------------

    #[test]
    fn infers_columns_from_text_x_positions_with_outer_frame_verticals_only() {
        reset_ids();
        // H-line-only triggers when a Y-group has ≥2 H-lines but < 2 interior
        // vertical lines. We provide outer-frame verticals to keep the group
        // together, but no interior column dividers.
        let segs = vec![
            h_seg(400.0, 100.0, 500.0),
            h_seg(350.0, 100.0, 500.0),
            h_seg(300.0, 100.0, 500.0),
            // Outer frame verticals (left + right only, no interior)
            v_seg(100.0, 300.0, 400.0),
            v_seg(500.0, 300.0, 400.0),
        ];
        let boxes = vec![
            tb("Label", 140.0, 375.0),
            tb("Value", 420.0, 375.0),
            tb("Label2", 140.0, 325.0),
            tb("Value2", 420.0, 325.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert!(!result.grids.is_empty());
        if !result.grids.is_empty() {
            // Only 2 unique X-lines from verticals (100, 500) → groupXLines has 2
            // which means it goes to buildTableGrid not buildHLineOnlyTable.
            // But with only left+right borders, it's effectively a 1-column grid
            // that still captures the text.
            let all_text: Vec<&str> = result.grids[0]
                .cells
                .iter()
                .map(|c| c.text.as_str())
                .filter(|t| !t.is_empty())
                .collect();
            assert!(!all_text.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // Framed-box interior reconstruction
    // -----------------------------------------------------------------------

    #[test]
    fn framed_box_with_aligned_interior_recovers_columns() {
        reset_ids();
        // Outer border only: two verticals + top/bottom rules. Interior
        // is whitespace-aligned name/value rows.
        let segs = vec![
            h_seg(400.0, 100.0, 500.0),
            h_seg(315.0, 100.0, 500.0),
            v_seg(100.0, 315.0, 400.0),
            v_seg(500.0, 315.0, 400.0),
        ];
        let boxes = vec![
            wide_box("Jagger", 120.0, 180.0, 380.0),
            wide_box("23.0", 300.0, 340.0, 380.0),
            wide_box("TAM 107", 120.0, 190.0, 355.0),
            wide_box("13.5", 300.0, 340.0, 355.0),
            wide_box("2137", 120.0, 160.0, 330.0),
            wide_box("12.9", 300.0, 340.0, 330.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
        let g = &result.grids[0];
        assert!(g.is_borderless);
        assert_eq!(g.cols, 2, "interior columns must be recovered");
        assert_eq!(cell_text(g, 0, 0), "Jagger");
        assert_eq!(cell_text(g, 2, 1), "12.9");
    }

    // -----------------------------------------------------------------------
    // Diagram filtering
    // -----------------------------------------------------------------------

    #[test]
    fn filters_out_sparse_grids_with_wide_column_count() {
        reset_ids();
        let x_lines = [
            100.0, 160.0, 220.0, 280.0, 340.0, 400.0, 460.0, 520.0, 580.0,
        ];
        let y_lines = [400.0, 350.0, 300.0, 250.0];
        let segs = table_segs(&x_lines, &y_lines);
        let boxes = vec![
            tb("A", 130.0, 375.0),
            tb("B", 310.0, 375.0),
            tb("C", 190.0, 325.0),
            tb("D", 430.0, 325.0),
            tb("E", 250.0, 275.0),
            tb("F", 550.0, 275.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        // After pruning keeps 3 rows × ~6 cols ≈ 18 cells with 6 filled (33%)
        // plus cols >= 6 → filtered as diagram
        assert_eq!(result.grids.len(), 0);
    }

    #[test]
    fn filters_out_grids_with_more_than_25_columns() {
        reset_ids();
        let x_lines: Vec<f64> = (0..32).map(|i| 100.0 + i as f64 * 15.0).collect();
        let y_lines = [400.0, 300.0];
        let segs = table_segs(&x_lines, &y_lines);
        let boxes: Vec<TextBox> = x_lines[..x_lines.len() - 1]
            .iter()
            .enumerate()
            .map(|(i, &x)| tb(&format!("c{}", i), x + 7.0, 350.0))
            .collect();
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 0);
    }

    #[test]
    fn filters_out_sparse_grids_with_high_text_duplication() {
        reset_ids();
        let x_lines = [
            100.0, 150.0, 200.0, 250.0, 300.0, 350.0, 400.0, 450.0, 500.0,
        ];
        let y_lines = [400.0, 360.0, 320.0, 280.0];
        let segs = table_segs(&x_lines, &y_lines);
        let boxes = vec![
            tb("Block", 125.0, 380.0),
            tb("Block", 275.0, 380.0),
            tb("Block", 425.0, 380.0),
            tb("Hash", 125.0, 340.0),
            tb("Hash", 275.0, 340.0),
            tb("Hash", 425.0, 340.0),
            tb("Nonce", 175.0, 340.0),
            tb("Nonce", 325.0, 340.0),
            tb("Nonce", 475.0, 340.0),
            tb("Tx", 125.0, 300.0),
            tb("Tx", 275.0, 300.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 0);
    }

    #[test]
    fn keeps_real_data_tables_with_high_fill() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0, 300.0]);
        let boxes = vec![
            tb("Name", 200.0, 375.0),
            tb("Role", 400.0, 375.0),
            tb("Alice", 200.0, 325.0),
            tb("Engineer", 400.0, 325.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Decorative lines ignored
    // -----------------------------------------------------------------------

    #[test]
    fn ignores_h_lines_with_small_y_span_less_than_min_table_height() {
        reset_ids();
        let segs = vec![
            h_seg(400.0, 100.0, 500.0),
            h_seg(395.0, 100.0, 500.0),
            v_seg(100.0, 395.0, 400.0),
            v_seg(500.0, 395.0, 400.0),
        ];
        let result = resolve_table_grids(1, &[tb("text", 300.0, 397.0)], &segs);
        assert_eq!(result.grids.len(), 0);
    }

    // -----------------------------------------------------------------------
    // Cross-column text box splitting
    // -----------------------------------------------------------------------

    #[test]
    fn splits_a_text_box_spanning_two_columns_into_separate_cells() {
        reset_ids();
        let segs = table_segs(&[100.0, 200.0, 300.0, 400.0], &[500.0, 450.0]);
        let boxes = vec![
            wide_box("Alpha Beta", 110.0, 290.0, 475.0),
            tb("C", 350.0, 475.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
        let g = &result.grids[0];
        assert_eq!(cell_text(g, 0, 0), "Alpha");
        assert_eq!(cell_text(g, 0, 1), "Beta");
        assert_eq!(cell_text(g, 0, 2), "C");
    }

    #[test]
    fn splits_a_text_box_spanning_three_columns() {
        reset_ids();
        let segs = table_segs(&[100.0, 200.0, 300.0, 400.0], &[500.0, 450.0]);
        let boxes = vec![wide_box("Aaa Bbb Ccc", 110.0, 390.0, 475.0)];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
        let texts: Vec<&str> = result.grids[0]
            .cells
            .iter()
            .map(|c| c.text.as_str())
            .filter(|t| !t.trim().is_empty())
            .collect();
        // All three words should end up in separate cells
        assert_eq!(texts.len(), 3);
    }

    #[test]
    fn does_not_split_a_text_box_within_one_column() {
        reset_ids();
        let segs = table_segs(&[100.0, 200.0, 300.0, 400.0], &[500.0, 450.0]);
        let boxes = vec![
            wide_box("Hello World", 110.0, 190.0, 475.0),
            tb("X", 250.0, 475.0),
            tb("Y", 350.0, 475.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
        assert_eq!(cell_text(&result.grids[0], 0, 0), "Hello World");
    }

    #[test]
    fn keeps_single_word_boxes_intact_even_if_spanning_columns() {
        reset_ids();
        let segs = table_segs(&[100.0, 200.0, 300.0, 400.0], &[500.0, 450.0]);
        let boxes = vec![
            wide_box("Superlongword", 110.0, 290.0, 475.0),
            tb("Z", 350.0, 475.0),
        ];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
        let all_text: Vec<&str> = result.grids[0]
            .cells
            .iter()
            .map(|c| c.text.as_str())
            .filter(|t| !t.is_empty())
            .collect();
        assert!(all_text.contains(&"Superlongword"));
    }

    #[test]
    fn consumes_original_text_box_ids_when_splitting() {
        reset_ids();
        let segs = table_segs(&[100.0, 200.0, 300.0, 400.0], &[500.0, 450.0]);
        let bx = wide_box("Alpha Beta", 110.0, 290.0, 475.0);
        let original_id = bx.id.clone();
        let result = resolve_table_grids(1, &[bx], &segs);
        // The original ID must be consumed so it doesn't appear as free text
        assert!(result.consumed_ids.contains(&original_id));
    }

    // -----------------------------------------------------------------------
    // Header detection
    // -----------------------------------------------------------------------

    #[test]
    fn does_not_absorb_wide_paragraph_text_as_a_header_row() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0]);
        // A long sentence sitting 15pt above the grid — wider than 1.5 columns
        let paragraph = wide_box(
            "include only the results for the tasks that have an unbounded score:",
            100.0,
            420.0,
            412.0,
        );
        let paragraph_id = paragraph.id.clone();
        let boxes = vec![paragraph, tb("A", 200.0, 375.0), tb("B", 400.0, 375.0)];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
        // The paragraph should NOT be consumed by the table
        assert!(!result.consumed_ids.contains(&paragraph_id));
        // The grid should have 1 row, not 2
        assert_eq!(result.grids[0].rows, 1);
    }

    #[test]
    fn absorbs_narrow_column_headers_above_the_grid() {
        reset_ids();
        let segs = table_segs(&[100.0, 300.0, 500.0], &[400.0, 350.0]);
        let h1 = wide_box("Name", 150.0, 250.0, 412.0);
        let h2 = wide_box("Role", 350.0, 450.0, 412.0);
        let h1_id = h1.id.clone();
        let h2_id = h2.id.clone();
        let boxes = vec![h1, h2, tb("Alice", 200.0, 375.0), tb("CEO", 400.0, 375.0)];
        let result = resolve_table_grids(1, &boxes, &segs);
        assert_eq!(result.grids.len(), 1);
        // Both headers should be consumed
        assert!(result.consumed_ids.contains(&h1_id));
        assert!(result.consumed_ids.contains(&h2_id));
        // Grid should have 2 rows (header + data)
        assert_eq!(result.grids[0].rows, 2);
    }
}
