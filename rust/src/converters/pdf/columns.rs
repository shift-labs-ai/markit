//! Multi-column layout detection and text box reordering.
//!
//! Reading order is recursive whitespace decomposition (an XY-cut with
//! crossing tolerance):
//!
//!   1. Split the region at every horizontal whitespace band tall
//!      enough to be structural (≥ 2.5 line heights) — stacked slices
//!      in top-to-bottom order.
//!   2. Within a slice, find vertical gutters via an x-coverage
//!      histogram: a gutter is a run of bins that almost no box
//!      crosses (full-width titles and headings may cross), wide
//!      enough, with enough boxes fully on each side.
//!   3. Boxes crossing a gutter are full-width "bands"; they partition
//!      the slice vertically. Each partition's columns are emitted
//!      left to right — and each column recurses, so a column may
//!      contain its own headings, sub-columns, and structure
//!      (magazine layouts).
//!
//! This only detects the structure. The caller is responsible for
//! processing each group's text boxes independently (table detection,
//! rendering, etc.).

use crate::converters::pdf::types::{Segment, TextBox};

/// Minimum number of text boxes fully on each side of a gutter.
const MIN_BOXES_PER_COLUMN: usize = 4;

/// Recursion cap for the layout tree (each level is a horizontal
/// slice or a column).
const MAX_LAYOUT_DEPTH: usize = 6;

/// A horizontal whitespace band must be at least this tall in points…
const H_SPLIT_MIN_GAP_PTS: f64 = 18.0;

/// …and at least this many median line-heights, to count as a
/// structural break rather than paragraph leading.
const H_SPLIT_GAP_LINES: f64 = 2.5;

/// Minimum gutter width in points at 10pt body text; scaled by the
/// region's median font size (dense 6pt encyclopedia columns sit
/// ~8pt apart, which is as structural at that scale as 12pt is at
/// 10pt).
const MIN_GUTTER_PTS: f64 = 12.0;

/// Fraction of the text width excluded at each edge when searching for
/// gutters — a gutter in the outer margins is ragged-edge whitespace,
/// not a column separator.
const GUTTER_SEARCH_MARGIN: f64 = 0.15;

/// Fraction of the page's LINES allowed to cross a gutter (full-width
/// titles, headings, footnote rules). More crossings than this means
/// the whitespace is coincidental, not structural. Lines, not boxes:
/// fragmented OCR text carries several boxes per line, and a
/// box-based allowance lets phantom gutters through.
const MAX_CROSSING_FRACTION: f64 = 0.15;

/// Maximum number of gutters (four-column magazine spreads). One
/// gutter is admitted on the side counts alone; two or more must also
/// pass the prose-interval proof in `find_gutters` — table whitespace
/// fails it.
const MAX_GUTTERS: usize = 3;

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

/// Gutter centers found via the crossing histogram; multi-gutter
/// results must pass the prose-interval proof.
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

    // Count distinct text lines (y-mid clusters) for the crossing
    // allowance.
    let mut mids: Vec<f64> = text_boxes
        .iter()
        .map(|tb| (tb.bounds.top + tb.bounds.bottom) / 2.0)
        .collect();
    mids.sort_by(|a, b| b.total_cmp(a));
    let mut lines = 0usize;
    let mut last_mid = f64::INFINITY;
    for mid in mids {
        if (last_mid - mid).abs() > 3.0 {
            lines += 1;
            last_mid = mid;
        }
    }
    let max_crossing = 1.max((lines as f64 * MAX_CROSSING_FRACTION) as usize);

    let mut sizes: Vec<f64> = text_boxes.iter().map(|tb| tb.font_size).collect();
    sizes.sort_by(|a, b| a.total_cmp(b));
    let median_size = sizes.get(sizes.len() / 2).copied().unwrap_or(10.0);
    let min_gutter = (MIN_GUTTER_PTS * median_size / 10.0).clamp(6.0, MIN_GUTTER_PTS);

    // Two crossing histograms via difference arrays. The strict one
    // counts only box EDGES (a line poking a few points across a
    // gutter is evidence against it); centered titles and author
    // blocks extending far beyond both sides are band candidates that
    // the partitioning below handles, so their interiors are exempt
    // from the strict count — but a looser FULL-span cap still
    // disqualifies bins inside ordinary body text.
    let band_exempt = 2.5 * min_gutter;
    let bins = (hi - lo + 1) as usize;
    let mut poke_diff = vec![0i32; bins + 1];
    let mut full_diff = vec![0i32; bins + 1];
    {
        let add = |diff: &mut Vec<i32>, from: f64, to: f64| {
            let from = (from.floor() as i64 + 1).max(lo);
            let to = (to.ceil() as i64 - 1).min(hi);
            if from <= to {
                diff[(from - lo) as usize] += 1;
                diff[(to - lo + 1) as usize] -= 1;
            }
        };
        for tb in text_boxes {
            // Box crosses bin x when left + 2 < x < right - 2.
            let l = tb.bounds.left + 2.0;
            let r = tb.bounds.right - 2.0;
            add(&mut full_diff, l, r);
            if r - l <= 2.0 * band_exempt {
                add(&mut poke_diff, l, r);
            } else {
                add(&mut poke_diff, l, l + band_exempt);
                add(&mut poke_diff, r - band_exempt, r);
            }
        }
    }
    let max_full = 1.max((lines as f64 * 0.25) as usize) as i32;

    // Runs of bins where pokes stay under the strict allowance and
    // full crossings under the loose cap.
    let mut runs: Vec<(i64, i64)> = Vec::new();
    let mut run_start: Option<i64> = None;
    let mut poke = 0i32;
    let mut full = 0i32;
    for bin in 0..bins {
        poke += poke_diff[bin];
        full += full_diff[bin];
        let x = lo + bin as i64;
        if poke <= max_crossing as i32 && full <= max_full {
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
        // k clear integer bins span ~k+1 points of whitespace: the
        // bordering bins are each partially clear too.
        let run_width = end - start + 2;
        if (run_width as f64) < min_gutter {
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
        // Rank candidates by balance: a real column gutter has
        // substantial text fully on BOTH sides, while clear zones
        // inside figures are lopsided. (Run width misleads: figure
        // whitespace is often wider than a tight column gutter.)
        centers.push((center, left_count.min(right_count) as i64));
    }

    // A single qualifying gutter is the classic two-column case: the
    // side counts above already validated it.
    if centers.len() <= 1 {
        return centers.into_iter().map(|(c, _)| c).collect();
    }

    // Multiple candidates include phantoms (clear zones inside
    // figures, table whitespace). Every admitted gutter set must pass
    // the prose-interval proof over ALL its intervals including the
    // outer flanks: real columns are filled by wide line boxes; table
    // cells and figure labels sit narrow. Prefer the largest passing
    // subset, trying wider gutters first.
    centers.sort_by_key(|c| std::cmp::Reverse(c.1));
    centers.truncate(MAX_GUTTERS);
    let prose_intervals = |xs: &[f64]| -> bool {
        let mut pts = Vec::with_capacity(xs.len() + 2);
        pts.push(x_min - 1.0);
        pts.extend_from_slice(xs);
        pts.push(x_max + 1.0);
        pts.windows(2).all(|pair| {
            // The interval must hold real column lines: boxes filling
            // at least half its width. Narrow noise (figure labels,
            // axis numbers) doesn't disqualify — but table cells never
            // fill their interval, so table whitespace still fails.
            let span = pair[1] - pair[0];
            text_boxes
                .iter()
                .filter(|tb| {
                    tb.bounds.left >= pair[0]
                        && tb.bounds.right <= pair[1]
                        && tb.bounds.right - tb.bounds.left >= 0.5 * span
                })
                .count()
                >= MIN_BOXES_PER_COLUMN
        })
    };
    let candidates: Vec<f64> = centers.iter().map(|(c, _)| c).copied().collect();
    let subsets: Vec<Vec<usize>> = match candidates.len() {
        3 => vec![
            vec![0, 1, 2],
            vec![0, 1],
            vec![0, 2],
            vec![1, 2],
            vec![0],
            vec![1],
            vec![2],
        ],
        2 => vec![vec![0, 1], vec![0], vec![1]],
        _ => vec![vec![0]],
    };
    for subset in subsets {
        let mut xs: Vec<f64> = subset.iter().map(|&i| candidates[i]).collect();
        xs.sort_by(|a, b| a.total_cmp(b));
        if prose_intervals(&xs) {
            return xs;
        }
    }
    vec![]
}

/// Does a ruled grid (≥2 vertical and ≥2 horizontal segments) cover
/// most of the region's text? Such a region is a table and must stay
/// whole — but a figure's box elsewhere on the page must not freeze
/// column detection, so coverage of the text matters, not presence.
fn region_has_ruled_grid(boxes: &[TextBox], segments: &[Segment]) -> bool {
    if segments.len() < 4 || boxes.is_empty() {
        return false;
    }
    let x_min = boxes
        .iter()
        .map(|tb| tb.bounds.left)
        .fold(f64::INFINITY, f64::min);
    let x_max = boxes
        .iter()
        .map(|tb| tb.bounds.right)
        .fold(f64::NEG_INFINITY, f64::max);
    let y_min = boxes
        .iter()
        .map(|tb| tb.bounds.bottom)
        .fold(f64::INFINITY, f64::min);
    let y_max = boxes
        .iter()
        .map(|tb| tb.bounds.top)
        .fold(f64::NEG_INFINITY, f64::max);
    let overlaps = |seg: &Segment| -> bool {
        seg.x1.max(seg.x2) >= x_min
            && seg.x1.min(seg.x2) <= x_max
            && seg.y1.max(seg.y2) >= y_min
            && seg.y1.min(seg.y2) <= y_max
    };
    let vertical: Vec<&Segment> = segments
        .iter()
        .filter(|s| (s.x1 - s.x2).abs() <= 1.0 && overlaps(s))
        .collect();
    let horizontal: Vec<&Segment> = segments
        .iter()
        .filter(|s| (s.y1 - s.y2).abs() <= 1.0 && overlaps(s))
        .collect();
    if vertical.len() < 2 || horizontal.len() < 2 {
        return false;
    }
    // Grid bbox: union of the qualifying segments.
    let all = vertical.iter().chain(horizontal.iter());
    let (mut gx0, mut gy0, mut gx1, mut gy1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for seg in all {
        gx0 = gx0.min(seg.x1.min(seg.x2));
        gx1 = gx1.max(seg.x1.max(seg.x2));
        gy0 = gy0.min(seg.y1.min(seg.y2));
        gy1 = gy1.max(seg.y1.max(seg.y2));
    }
    let inside = boxes
        .iter()
        .filter(|tb| {
            let cx = (tb.bounds.left + tb.bounds.right) / 2.0;
            let cy = (tb.bounds.top + tb.bounds.bottom) / 2.0;
            cx >= gx0 && cx <= gx1 && cy >= gy0 && cy <= gy1
        })
        .count();
    inside * 10 >= boxes.len() * 6
}

/// Median glyph-box height — the line-height reference for horizontal
/// splitting.
fn median_height(boxes: &[TextBox]) -> f64 {
    let mut heights: Vec<f64> = boxes
        .iter()
        .map(|tb| tb.bounds.top - tb.bounds.bottom)
        .filter(|h| *h > 0.0)
        .collect();
    if heights.is_empty() {
        return 10.0;
    }
    heights.sort_by(|a, b| a.total_cmp(b));
    heights[heights.len() / 2]
}

/// Does a vertical segment span the whitespace band [lower, upper]?
/// A table border crossing the gap proves the region is one object and
/// must not be sliced.
fn vertical_segment_spans_gap(segments: &[Segment], lower: f64, upper: f64) -> bool {
    segments.iter().any(|seg| {
        (seg.x1 - seg.x2).abs() <= 1.0 && {
            let seg_min = seg.y1.min(seg.y2);
            let seg_max = seg.y1.max(seg.y2);
            seg_min < lower + 1.0 && seg_max > upper - 1.0
        }
    })
}

/// Split a region at horizontal whitespace bands taller than the
/// structural threshold. Takes ownership to avoid deep-cloning boxes
/// at every recursion level; gives them back when the region is one
/// piece.
#[allow(clippy::result_large_err)]
fn horizontal_splits(
    mut boxes: Vec<TextBox>,
    segments: &[Segment],
) -> Result<Vec<Vec<TextBox>>, Vec<TextBox>> {
    let threshold = (H_SPLIT_GAP_LINES * median_height(&boxes)).max(H_SPLIT_MIN_GAP_PTS);
    boxes.sort_by(|a, b| b.bounds.top.total_cmp(&a.bounds.top));

    let mut parts: Vec<Vec<TextBox>> = Vec::new();
    let mut current: Vec<TextBox> = Vec::new();
    let mut min_bottom = f64::INFINITY;
    for tb in boxes {
        if !current.is_empty()
            && min_bottom - tb.bounds.top >= threshold
            && !vertical_segment_spans_gap(segments, tb.bounds.top, min_bottom)
        {
            parts.push(std::mem::take(&mut current));
            min_bottom = f64::INFINITY;
        }
        min_bottom = min_bottom.min(tb.bounds.bottom);
        current.push(tb);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.len() >= 2 {
        Ok(parts)
    } else {
        Err(parts.pop().unwrap_or_default())
    }
}

/// Cell-gap threshold and width cap for the tabular-region check.
const TABULAR_CELL_GAP: f64 = 15.0;
const TABULAR_MAX_FRAGMENT_FRACTION: f64 = 0.18;
const TABULAR_MIN_ROWS: usize = 3;
const TABULAR_MIN_FRAGMENTS: usize = 3;

/// Does the region read as an unruled table? Rows of ≥3 narrow,
/// well-separated fragments are data rows — text columns produce at
/// most one wide fragment per column. Such regions must stay whole for
/// table detection instead of being split into false page columns.
fn region_is_tabular(boxes: &[TextBox]) -> bool {
    let x_min = boxes
        .iter()
        .map(|tb| tb.bounds.left)
        .fold(f64::INFINITY, f64::min);
    let x_max = boxes
        .iter()
        .map(|tb| tb.bounds.right)
        .fold(f64::NEG_INFINITY, f64::max);
    let width = x_max - x_min;
    if width <= 0.0 {
        return false;
    }
    let max_fragment = width * TABULAR_MAX_FRAGMENT_FRACTION;

    // Group into visual rows by Y midpoint.
    let mut sorted: Vec<&TextBox> = boxes.iter().collect();
    sorted.sort_by(|a, b| {
        let ya = (a.bounds.top + a.bounds.bottom) / 2.0;
        let yb = (b.bounds.top + b.bounds.bottom) / 2.0;
        yb.total_cmp(&ya)
    });
    let mut rows: Vec<Vec<&TextBox>> = Vec::new();
    let mut row_mid = f64::NEG_INFINITY;
    for tb in sorted {
        let mid = (tb.bounds.top + tb.bounds.bottom) / 2.0;
        if rows.is_empty() || (row_mid - mid).abs() > 3.0 {
            rows.push(vec![tb]);
            row_mid = mid;
        } else {
            rows.last_mut().unwrap().push(tb);
        }
    }

    let mut qualifying = 0usize;
    for row in &rows {
        let mut row_boxes: Vec<&&TextBox> = row.iter().collect();
        row_boxes.sort_by(|a, b| a.bounds.left.total_cmp(&b.bounds.left));
        // Cluster into fragments by cell gap.
        let mut fragments: Vec<(f64, f64)> = Vec::new();
        for tb in row_boxes {
            match fragments.last_mut() {
                Some(fragment) if tb.bounds.left - fragment.1 < TABULAR_CELL_GAP => {
                    fragment.1 = fragment.1.max(tb.bounds.right);
                }
                _ => fragments.push((tb.bounds.left, tb.bounds.right)),
            }
        }
        if fragments.len() >= TABULAR_MIN_FRAGMENTS
            && fragments.iter().all(|(l, r)| r - l <= max_fragment)
        {
            qualifying += 1;
        }
    }
    qualifying >= TABULAR_MIN_ROWS && qualifying * 3 >= rows.len()
}

/// Recursive layout: horizontal slices first, then tolerant gutter
/// detection, recursing into each column. `is_band` records the leaf's
/// provenance — horizontal slices and crossing boxes are full-width
/// bands, column leaves are not.
fn layout_region(
    boxes: Vec<TextBox>,
    segments: &[Segment],
    depth: usize,
    is_band: bool,
    out: &mut Vec<(Vec<TextBox>, bool)>,
    gutters_out: &mut Vec<f64>,
) {
    if depth >= MAX_LAYOUT_DEPTH || boxes.len() < MIN_BOXES_PER_COLUMN * 2 {
        out.push((boxes, is_band));
        return;
    }

    let boxes = match horizontal_splits(boxes, segments) {
        Ok(parts) => {
            for part in parts {
                layout_region(part, segments, depth + 1, true, out, gutters_out);
            }
            return;
        }
        Err(boxes) => boxes,
    };

    // A ruled table region must not be column-split — its interior
    // whitespace belongs to the grid, not the page layout. Neither may
    // an unruled region whose rows read as data cells.
    if region_has_ruled_grid(&boxes, segments) || region_is_tabular(&boxes) {
        out.push((boxes, is_band));
        return;
    }

    let gutters = find_gutters(&boxes);
    if gutters.is_empty() {
        out.push((boxes, is_band));
        return;
    }
    gutters_out.extend(gutters.iter().copied());

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

    // Walk top-to-bottom (Y-up: larger top first). Crossing boxes
    // (bands) flush the open partition; consecutive band boxes group.
    let mut ordered = boxes;
    ordered.sort_by(|a, b| b.bounds.top.total_cmp(&a.bounds.top));

    let mut region_columns: Vec<Vec<TextBox>> = vec![Vec::new(); gutters.len() + 1];
    let mut open_band: Vec<TextBox> = Vec::new();

    // Local flush helpers expressed as small closures over `out`.
    fn flush_columns(
        region_columns: &mut [Vec<TextBox>],
        segments: &[Segment],
        depth: usize,
        out: &mut Vec<(Vec<TextBox>, bool)>,
        gutters_out: &mut Vec<f64>,
    ) {
        for column in region_columns.iter_mut() {
            if !column.is_empty() {
                layout_region(
                    std::mem::take(column),
                    segments,
                    depth + 1,
                    false,
                    out,
                    gutters_out,
                );
            }
        }
    }

    for tb in ordered {
        if crosses_gutter(&tb) {
            flush_columns(&mut region_columns, segments, depth, out, gutters_out);
            open_band.push(tb);
        } else {
            if !open_band.is_empty() {
                out.push((std::mem::take(&mut open_band), true));
            }
            region_columns[column_of(&tb)].push(tb);
        }
    }
    flush_columns(&mut region_columns, segments, depth, out, gutters_out);
    if !open_band.is_empty() {
        out.push((open_band, true));
    }
}

/// Detect column layout and return text boxes grouped in reading order.
///
/// For single-column pages, returns all boxes in one group. For
/// structured pages, returns full-width bands, horizontal slices, and
/// per-region columns (recursively decomposed) in reading order.
pub fn detect_columns(text_boxes: &[TextBox], segments: &[Segment]) -> ColumnLayout {
    if text_boxes.len() < MIN_BOXES_PER_COLUMN * 2 {
        return single(text_boxes);
    }

    let mut groups: Vec<(Vec<TextBox>, bool)> = Vec::new();
    let mut gutters: Vec<f64> = Vec::new();
    layout_region(
        text_boxes.to_vec(),
        segments,
        0,
        false,
        &mut groups,
        &mut gutters,
    );

    if groups.len() <= 1 {
        return single(text_boxes);
    }
    gutters.sort_by(|a, b| a.total_cmp(b));
    gutters.dedup_by(|a, b| (*a - *b).abs() < 1.0);

    let (columns, bands): (Vec<Vec<TextBox>>, Vec<bool>) = groups.into_iter().unzip();
    ColumnLayout {
        column_count: columns.len(),
        columns,
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
        let result = detect_columns(&boxes, &[]);
        assert_eq!(result.column_count, 1);
        assert_eq!(result.columns.len(), 1);
    }

    #[test]
    fn returns_1_column_for_single_column_layout() {
        let boxes: Vec<TextBox> = (0..20)
            .map(|i| tb_default(&format!("Line {i}"), 72.0, 700.0 - i as f64 * 15.0))
            .collect();
        let result = detect_columns(&boxes, &[]);
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
        let result = detect_columns(&combined, &[]);
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
        let result = detect_columns(&boxes, &[]);
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
        let result = detect_columns(&combined, &[]);
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
        let result = detect_columns(&combined, &[]);
        assert_eq!(result.column_count, 1);
    }

    #[test]
    fn does_not_split_when_one_side_has_too_few_boxes() {
        let left: Vec<TextBox> = (0..15)
            .map(|i| tb_default(&format!("Main {i}"), 72.0, 700.0 - i as f64 * 15.0))
            .collect();
        let right = vec![tb_default("Margin note", 400.0, 600.0)]; // only 1 box on right
        let combined: Vec<TextBox> = left.into_iter().chain(right).collect();
        let result = detect_columns(&combined, &[]);
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
        let result = detect_columns(&combined, &[]);
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
    fn nested_headings_stay_inside_their_own_column() {
        // Magazine layout: two columns, each with its own heading above
        // its own body. The flat model interleaved the headings into one
        // line; the recursive cut keeps each heading with its column.
        let left_heading = tb("QUIENES SOMOS", 72.0, 700.0, 180.0);
        let left_body: Vec<TextBox> = (0..6)
            .map(|i| tb(&format!("L{i}"), 72.0, 680.0 - i as f64 * 15.0, 200.0))
            .collect();
        let right_heading = tb("NUESTRO IMPACTO", 315.0, 700.0, 180.0);
        let right_body: Vec<TextBox> = (0..6)
            .map(|i| tb(&format!("R{i}"), 315.0, 680.0 - i as f64 * 15.0, 200.0))
            .collect();
        let combined: Vec<TextBox> = [left_heading]
            .into_iter()
            .chain(left_body)
            .chain([right_heading])
            .chain(right_body)
            .collect();
        let result = detect_columns(&combined, &[]);
        // Two groups: the whole left column (heading + body), then the
        // whole right column.
        assert_eq!(result.column_count, 2);
        let texts: Vec<Vec<&str>> = result
            .columns
            .iter()
            .map(|g| g.iter().map(|b| b.text.as_str()).collect())
            .collect();
        assert_eq!(texts[0][0], "QUIENES SOMOS");
        assert!(texts[0][1..].iter().all(|t| t.starts_with('L')));
        assert_eq!(texts[1][0], "NUESTRO IMPACTO");
        assert!(texts[1][1..].iter().all(|t| t.starts_with('R')));
    }

    #[test]
    fn four_column_spread_is_admitted_with_interval_proof() {
        // Wide landscape spread: four prose columns whose line boxes
        // fill their intervals.
        let boxes: Vec<TextBox> = [40.0, 300.0, 560.0, 820.0]
            .into_iter()
            .enumerate()
            .flat_map(|(column, x)| {
                (0..6)
                    .map(move |i| tb(&format!("C{column}-{i}"), x, 700.0 - i as f64 * 15.0, 220.0))
            })
            .collect();
        let result = detect_columns(&boxes, &[]);
        assert_eq!(result.column_count, 4);
        assert_eq!(result.boundaries.len(), 3);
    }

    #[test]
    fn table_whitespace_with_multiple_gutters_stays_whole() {
        // Four columns of NARROW cells (numbers) — the same gutter
        // geometry as a spread, but the interval proof fails because
        // the boxes do not fill their intervals.
        let boxes: Vec<TextBox> = [40.0, 300.0, 560.0, 820.0]
            .into_iter()
            .enumerate()
            .flat_map(|(column, x)| {
                (0..6).map(move |i| tb(&format!("{column}{i}"), x, 700.0 - i as f64 * 15.0, 40.0))
            })
            .collect();
        let result = detect_columns(&boxes, &[]);
        assert_eq!(result.column_count, 1, "{:?}", result.boundaries);
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
        let result = detect_columns(&combined, &[]);
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
