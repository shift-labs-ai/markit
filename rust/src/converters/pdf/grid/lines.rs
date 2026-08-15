//! Table-line grouping and row expansion: deduplicate coordinates,
//! split disconnected horizontal-line groups using vertical bridges,
//! and subdivide rows by text Y-clusters.

use std::collections::{HashMap, HashSet};

use crate::converters::pdf::types::{Segment, TableCell, TextBox};

pub(super) fn unique_sorted(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut result: Vec<f64> = Vec::new();
    for v in sorted {
        if result.is_empty() || (result.last().unwrap() - v).abs() > 1.0 {
            result.push(v);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Y-line group splitting
// ---------------------------------------------------------------------------

/// `intervals` must be sorted by start. Same chain walk as the original
/// per-query implementation, minus the per-query clone and sort.
fn chain_covers_sorted(sorted: &[(f64, f64)], lower_y: f64, upper_y: f64, eps: f64) -> bool {
    let mut covered = lower_y;
    for iv in sorted {
        if iv.0 > covered + eps {
            break;
        }
        if iv.1 > covered {
            covered = iv.1;
        }
        if covered >= upper_y - eps {
            return true;
        }
    }
    false
}

/// Vertical segments grouped by rounded x, each group's y-intervals
/// sorted by start with cached extremes. Built once per page: the old
/// path rebuilt this map — and re-sorted every interval list — for
/// every adjacent y-line pair, which was quadratic on rule-dense
/// register manuals.
struct XIntervals {
    x: i64,
    intervals: Vec<(f64, f64)>,
    min_start: f64,
    max_end: f64,
}

fn vertical_index(verticals: &[Segment]) -> Vec<XIntervals> {
    let mut by_x: HashMap<i64, Vec<(f64, f64)>> = HashMap::new();
    for seg in verticals {
        let rx = seg.x1.round() as i64;
        by_x.entry(rx)
            .or_default()
            .push((seg.y1.min(seg.y2), seg.y1.max(seg.y2)));
    }
    let mut index: Vec<XIntervals> = by_x
        .into_iter()
        .map(|(x, mut intervals)| {
            intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let min_start = intervals.first().map_or(f64::INFINITY, |iv| iv.0);
            let max_end = intervals
                .iter()
                .fold(f64::NEG_INFINITY, |acc, iv| acc.max(iv.1));
            XIntervals {
                x,
                intervals,
                min_start,
                max_end,
            }
        })
        .collect();
    index.sort_by_key(|entry| entry.x);
    index
}

fn bridging_x_set(upper_y: f64, lower_y: f64, index: &[XIntervals]) -> HashSet<i64> {
    let eps = 1.5;
    let mut xs = HashSet::new();
    for entry in index {
        // Exact pre-filters: the chain starts at lower_y, so it needs a
        // first interval within eps of it, and covered can never exceed
        // max(lower_y, max_end).
        if entry.min_start > lower_y + eps {
            continue;
        }
        if entry.max_end < upper_y - eps && lower_y < upper_y - eps {
            continue;
        }
        if chain_covers_sorted(&entry.intervals, lower_y, upper_y, eps) {
            xs.insert(entry.x);
        }
    }
    xs
}

const MIN_RICH_BRIDGING_COLS: usize = 3;

pub(super) fn split_y_lines_into_groups(y_lines: &[f64], verticals: &[Segment]) -> Vec<Vec<f64>> {
    if y_lines.is_empty() {
        return vec![];
    }

    let eps = 1.5_f64;
    let all_x: Vec<i64> = verticals.iter().map(|s| s.x1.round() as i64).collect();
    let global_x_min = all_x.iter().copied().min().unwrap_or(0);
    let global_x_max = all_x.iter().copied().max().unwrap_or(0);

    let mut groups: Vec<Vec<f64>> = Vec::new();
    let mut current_group = vec![y_lines[0]];
    let mut prev_bridging_cols: i64 = -1;

    let index = vertical_index(verticals);
    for i in 1..y_lines.len() {
        let upper_y = y_lines[i - 1];
        let lower_y = y_lines[i];

        let bridging_xs = bridging_x_set(upper_y, lower_y, &index);
        let cols = bridging_xs.len();
        if cols == 0 {
            groups.push(current_group);
            current_group = vec![y_lines[i]];
            prev_bridging_cols = -1;
            continue;
        }

        if prev_bridging_cols >= MIN_RICH_BRIDGING_COLS as i64
            && (cols as i64) < MIN_RICH_BRIDGING_COLS as i64
        {
            let is_outer_frame_only = bridging_xs.iter().all(|&x| {
                (x - global_x_min).abs() as f64 <= eps || (x - global_x_max).abs() as f64 <= eps
            });
            if !is_outer_frame_only {
                groups.push(current_group);
                current_group = vec![y_lines[i - 1], y_lines[i]];
                prev_bridging_cols = cols as i64;
                continue;
            }
        }

        current_group.push(y_lines[i]);
        prev_bridging_cols = cols as i64;
    }

    groups.push(current_group);
    groups
}

// ---------------------------------------------------------------------------
// Sub-row Y-cluster expansion
// ---------------------------------------------------------------------------

const Y_CLUSTER_GAP: f64 = 10.0;
const MIN_COLS_IN_TOP_CLUSTER: usize = 2;

fn assign_to_y_cluster(y: f64, clusters: &[f64]) -> usize {
    let mut closest = 0;
    let mut closest_dist = (y - clusters[0]).abs();
    for k in 1..clusters.len() {
        let d = (y - clusters[k]).abs();
        if d < closest_dist {
            closest_dist = d;
            closest = k;
        }
    }
    closest
}

pub(super) fn expand_sub_rows_by_y_clusters(
    original_rows: usize,
    cols: usize,
    cells: &mut Vec<TableCell>,
    cell_boxes: &mut HashMap<usize, Vec<usize>>, // cell_index -> box indices into a shared vec
    all_boxes: &[TextBox],
) -> usize {
    // Streaming rewrite of the original in-place expansion: identical
    // per-row decisions (clusters, guards, redistribution), but rows
    // are emitted once into fresh cell storage instead of shifting and
    // rescanning the whole grid per split — the old path was
    // O(rows² × cols) and dominated conversion time on rule-heavy pages.
    let mut by_pos: HashMap<(usize, usize), (TableCell, Vec<usize>)> = HashMap::new();
    for (idx, cell) in cells.drain(..).enumerate() {
        let boxes = cell_boxes.remove(&idx).unwrap_or_default();
        by_pos.insert((cell.row, cell.col), (cell, boxes));
    }
    cell_boxes.clear();

    let mut out_cells: Vec<TableCell> = Vec::new();
    let mut out_boxes: HashMap<usize, Vec<usize>> = HashMap::new();
    let push_cell = |out_cells: &mut Vec<TableCell>,
                     out_boxes: &mut HashMap<usize, Vec<usize>>,
                     mut cell: TableCell,
                     row: usize,
                     boxes: Vec<usize>| {
        cell.row = row;
        if !boxes.is_empty() {
            out_boxes.insert(out_cells.len(), boxes);
        }
        out_cells.push(cell);
    };

    let mut out_row = 0usize;
    for orig_row in 0..original_rows {
        // Gather this row's cells and their boxes.
        let mut row_cells: Vec<(usize, TableCell, Vec<usize>)> = Vec::new();
        for col in 0..cols {
            if let Some((cell, boxes)) = by_pos.remove(&(orig_row, col)) {
                row_cells.push((col, cell, boxes));
            }
        }

        let non_empty: Vec<usize> = row_cells
            .iter()
            .filter(|(_, _, boxes)| !boxes.is_empty())
            .map(|(col, _, _)| *col)
            .collect();

        // Cluster the row's box mid-Ys (descending, gap > Y_CLUSTER_GAP).
        let clusters: Vec<f64> = {
            let mut sorted_y: Vec<f64> = row_cells
                .iter()
                .flat_map(|(_, _, boxes)| {
                    boxes.iter().map(|&bi| {
                        let y = (all_boxes[bi].bounds.top + all_boxes[bi].bounds.bottom) / 2.0;
                        (y * 10.0).round() / 10.0
                    })
                })
                .collect();
            sorted_y.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
            sorted_y.dedup();
            let mut clusters: Vec<f64> = Vec::new();
            for y in sorted_y {
                match clusters.last() {
                    Some(last) if last - y <= Y_CLUSTER_GAP => {}
                    _ => clusters.push(y),
                }
            }
            clusters
        };

        let split = if non_empty.is_empty() || clusters.len() < 2 {
            false
        } else {
            let mut cols_in_top_cluster: HashSet<usize> = HashSet::new();
            for (col, _, boxes) in &row_cells {
                if boxes.iter().any(|&bi| {
                    assign_to_y_cluster(
                        (all_boxes[bi].bounds.top + all_boxes[bi].bounds.bottom) / 2.0,
                        &clusters,
                    ) == 0
                }) {
                    cols_in_top_cluster.insert(*col);
                }
            }
            let sparse_cols_have_multiple_boxes = row_cells
                .iter()
                .any(|(col, _, boxes)| !cols_in_top_cluster.contains(col) && boxes.len() > 1);
            cols_in_top_cluster.len() >= MIN_COLS_IN_TOP_CLUSTER
                && cols_in_top_cluster.len() < non_empty.len()
                && sparse_cols_have_multiple_boxes
        };

        if !split {
            for (_, cell, boxes) in row_cells {
                push_cell(&mut out_cells, &mut out_boxes, cell, out_row, boxes);
            }
            out_row += 1;
            continue;
        }

        let num_sub_rows = clusters.len();
        // Redistribute each column's boxes across the sub-rows; sub-row
        // 0 keeps the original cell (text and all), later sub-rows get
        // fresh empty cells.
        let mut sub_rows: Vec<Vec<(usize, TableCell, Vec<usize>)>> =
            (0..num_sub_rows).map(|_| Vec::new()).collect();
        for (col, cell, boxes) in row_cells {
            let mut groups: Vec<Vec<usize>> = vec![Vec::new(); num_sub_rows];
            for bi in boxes {
                let cy = (all_boxes[bi].bounds.top + all_boxes[bi].bounds.bottom) / 2.0;
                groups[assign_to_y_cluster(cy, &clusters)].push(bi);
            }
            for (sub, group) in groups.into_iter().enumerate() {
                let sub_cell = if sub == 0 {
                    cell.clone()
                } else {
                    TableCell {
                        row: 0,
                        col,
                        text: String::new(),
                        row_span: 1,
                        col_span: 1,
                    }
                };
                sub_rows[sub].push((col, sub_cell, group));
            }
        }
        for (sub, row) in sub_rows.into_iter().enumerate() {
            for (_, cell, boxes) in row {
                push_cell(&mut out_cells, &mut out_boxes, cell, out_row + sub, boxes);
            }
        }
        out_row += num_sub_rows;
    }

    // Cells beyond original_rows do not exist today; carry any
    // stragglers defensively at their shifted position.
    let added = out_row - original_rows.min(out_row);
    let mut stragglers: Vec<(TableCell, Vec<usize>)> = by_pos.into_values().collect();
    stragglers.sort_by_key(|(cell, _)| (cell.row, cell.col));
    for (cell, boxes) in stragglers {
        let row = cell.row + added;
        push_cell(&mut out_cells, &mut out_boxes, cell, row, boxes);
    }

    *cells = out_cells;
    *cell_boxes = out_boxes;
    out_row.max(original_rows)
}

// ---------------------------------------------------------------------------
// Cross-column text box splitting
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn vertical(id: &str, x: f64, top: f64, bottom: f64) -> Segment {
        Segment {
            id: id.into(),
            x1: x,
            y1: top,
            x2: x,
            y2: bottom,
        }
    }

    #[test]
    fn unique_sorted_deduplicates_within_one_point() {
        assert_eq!(unique_sorted(&[3.0, 1.8, 1.0, 9.0]), [1.0, 3.0, 9.0]);
    }

    #[test]
    fn no_vertical_bridge_splits_every_y_interval() {
        assert_eq!(
            split_y_lines_into_groups(&[100.0, 80.0, 60.0], &[]),
            [vec![100.0], vec![80.0], vec![60.0]]
        );
    }

    #[test]
    fn continuous_outer_borders_keep_y_lines_together() {
        let v = [
            vertical("left", 0.0, 100.0, 60.0),
            vertical("right", 100.0, 100.0, 60.0),
        ];
        assert_eq!(
            split_y_lines_into_groups(&[100.0, 80.0, 60.0], &v),
            [vec![100.0, 80.0, 60.0]]
        );
    }

    #[test]
    fn rich_to_sparse_interior_bridge_starts_a_new_group() {
        let v = [
            vertical("left", 0.0, 100.0, 80.0),
            vertical("middle", 50.0, 100.0, 60.0),
            vertical("right", 100.0, 100.0, 80.0),
        ];
        assert_eq!(
            split_y_lines_into_groups(&[100.0, 80.0, 60.0], &v),
            [vec![100.0, 80.0], vec![80.0, 60.0]]
        );
    }

    /// The indexed bridging query must agree with the original
    /// per-query implementation (clone + sort + chain walk) on
    /// pseudo-random segment layouts.
    #[test]
    fn indexed_bridging_matches_naive_reference() {
        fn naive_bridging(upper_y: f64, lower_y: f64, verticals: &[Segment]) -> HashSet<i64> {
            let eps = 1.5;
            let mut xs = HashSet::new();
            let mut by_x: HashMap<i64, Vec<(f64, f64)>> = HashMap::new();
            for seg in verticals {
                let rx = seg.x1.round() as i64;
                by_x.entry(rx)
                    .or_default()
                    .push((seg.y1.min(seg.y2), seg.y1.max(seg.y2)));
            }
            for (rx, intervals) in &by_x {
                let mut sorted = intervals.clone();
                sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                if chain_covers_sorted(&sorted, lower_y, upper_y, eps) {
                    xs.insert(*rx);
                }
            }
            xs
        }

        let mut state = 0x2545f4914f6cdd1d_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let segments: Vec<Segment> = (0..300)
            .map(|i| {
                let x = (next() * 40.0).floor() * 5.0;
                let top = next() * 200.0;
                let len = next() * 40.0;
                vertical(&format!("s{i}"), x, top + len, top)
            })
            .collect();
        let index = vertical_index(&segments);
        for _ in 0..200 {
            let a = next() * 200.0;
            let b = next() * 200.0;
            let (upper, lower) = (a.max(b), a.min(b));
            assert_eq!(
                bridging_x_set(upper, lower, &index),
                naive_bridging(upper, lower, &segments),
                "mismatch for band {lower}..{upper}"
            );
        }
    }

    #[test]
    fn cluster_assignment_chooses_nearest_and_breaks_ties_first() {
        let centers = [100.0, 80.0];
        assert_eq!(assign_to_y_cluster(96.0, &centers), 0);
        assert_eq!(assign_to_y_cluster(84.0, &centers), 1);
        assert_eq!(assign_to_y_cluster(90.0, &centers), 0);
    }
}
