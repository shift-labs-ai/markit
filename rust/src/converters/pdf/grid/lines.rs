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

fn chain_covers_range(intervals: &[(f64, f64)], lower_y: f64, upper_y: f64, eps: f64) -> bool {
    let mut sorted = intervals.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut covered = lower_y;
    for iv in &sorted {
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

fn bridging_x_set(upper_y: f64, lower_y: f64, verticals: &[Segment]) -> HashSet<i64> {
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
        if chain_covers_range(intervals, lower_y, upper_y, eps) {
            xs.insert(*rx);
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

    for i in 1..y_lines.len() {
        let upper_y = y_lines[i - 1];
        let lower_y = y_lines[i];

        let bridging_xs = bridging_x_set(upper_y, lower_y, verticals);
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
    let mut added_rows: usize = 0;

    for orig_row in 0..original_rows {
        let current_row = orig_row + added_rows;

        // Collect row cell infos
        struct RowCellInfo {
            #[allow(dead_code)] // parity with the TS struct shape
            cell_idx: usize,
            col: usize,
            box_indices: Vec<usize>,
        }
        let mut row_cell_infos: Vec<RowCellInfo> = Vec::new();
        for col in 0..cols {
            if let Some(cell_idx) = cells
                .iter()
                .position(|c| c.row == current_row && c.col == col)
            {
                if let Some(bi) = cell_boxes.get(&cell_idx) {
                    if !bi.is_empty() {
                        row_cell_infos.push(RowCellInfo {
                            cell_idx,
                            col,
                            box_indices: bi.clone(),
                        });
                    }
                }
            }
        }
        if row_cell_infos.is_empty() {
            continue;
        }

        let all_mid_ys: Vec<f64> = row_cell_infos
            .iter()
            .flat_map(|rci| {
                rci.box_indices
                    .iter()
                    .map(|&bi| (all_boxes[bi].bounds.top + all_boxes[bi].bounds.bottom) / 2.0)
            })
            .collect();

        let sorted_y: Vec<f64> = {
            let mut s: Vec<f64> = all_mid_ys
                .iter()
                .map(|y| (y * 10.0).round() / 10.0)
                .collect();
            s.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
            s.dedup();
            s
        };
        // sorted_y is descending
        let mut clusters = vec![sorted_y[0]];
        for i in 1..sorted_y.len() {
            if clusters.last().unwrap() - sorted_y[i] > Y_CLUSTER_GAP {
                clusters.push(sorted_y[i]);
            }
        }
        if clusters.len() < 2 {
            continue;
        }

        let mut cols_in_top_cluster: HashSet<usize> = HashSet::new();
        let mut total_non_empty_cols: HashSet<usize> = HashSet::new();
        for rci in &row_cell_infos {
            total_non_empty_cols.insert(rci.col);
            if rci.box_indices.iter().any(|&bi| {
                assign_to_y_cluster(
                    (all_boxes[bi].bounds.top + all_boxes[bi].bounds.bottom) / 2.0,
                    &clusters,
                ) == 0
            }) {
                cols_in_top_cluster.insert(rci.col);
            }
        }

        if cols_in_top_cluster.len() < MIN_COLS_IN_TOP_CLUSTER {
            continue;
        }
        if cols_in_top_cluster.len() >= total_non_empty_cols.len() {
            continue;
        }

        let sparse_cols_have_multiple_boxes = row_cell_infos
            .iter()
            .any(|rci| !cols_in_top_cluster.contains(&rci.col) && rci.box_indices.len() > 1);
        if !sparse_cols_have_multiple_boxes {
            continue;
        }

        let num_sub_rows = clusters.len();
        let num_new_rows = num_sub_rows - 1;

        // Shift rows down
        for cell in cells.iter_mut() {
            if cell.row > current_row {
                cell.row += num_new_rows;
            }
        }

        // Cell indices change as rows are inserted; preserve box
        // assignments by stable (row, col) coordinates, then rebuild the
        // index-keyed map after insertion.
        let mut boxes_by_pos: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
        for (&cell_idx, bi) in cell_boxes.iter() {
            let c = &cells[cell_idx];
            boxes_by_pos.insert((c.row, c.col), bi.clone());
        }
        cell_boxes.clear();

        // Add new sub-row cells
        for sub_row in 1..num_sub_rows {
            for col in 0..cols {
                cells.push(TableCell {
                    row: current_row + sub_row,
                    col,
                    text: String::new(),
                    row_span: 1,
                    col_span: 1,
                });
            }
        }

        // Redistribute boxes across sub-rows
        for rci in &row_cell_infos {
            let mut sub_row_box_groups: Vec<Vec<usize>> = vec![vec![]; num_sub_rows];
            for &bi in &rci.box_indices {
                let cy = (all_boxes[bi].bounds.top + all_boxes[bi].bounds.bottom) / 2.0;
                let cluster_idx = assign_to_y_cluster(cy, &clusters);
                sub_row_box_groups[cluster_idx].push(bi);
            }

            // Update orig cell (sub_row 0)
            if sub_row_box_groups[0].is_empty() {
                boxes_by_pos.remove(&(current_row, rci.col));
            } else {
                boxes_by_pos.insert((current_row, rci.col), sub_row_box_groups[0].clone());
            }

            for sub_row in 1..num_sub_rows {
                if !sub_row_box_groups[sub_row].is_empty() {
                    boxes_by_pos.insert(
                        (current_row + sub_row, rci.col),
                        sub_row_box_groups[sub_row].clone(),
                    );
                }
            }
        }

        // Rebuild cell_boxes from boxes_by_pos
        for (cell_idx, c) in cells.iter().enumerate() {
            if let Some(bi) = boxes_by_pos.get(&(c.row, c.col)) {
                cell_boxes.insert(cell_idx, bi.clone());
            }
        }

        added_rows += num_new_rows;
        // Don't clear boxes_by_pos, we already rebuilt cell_boxes
        continue;
    }

    // If we didn't enter the loop body that rebuilds, make sure cell_boxes is consistent
    // Actually it should be fine — we only modify cell_boxes inside the loop when clusters >= 2

    original_rows + added_rows
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

    #[test]
    fn cluster_assignment_chooses_nearest_and_breaks_ties_first() {
        let centers = [100.0, 80.0];
        assert_eq!(assign_to_y_cluster(96.0, &centers), 0);
        assert_eq!(assign_to_y_cluster(84.0, &centers), 1);
        assert_eq!(assign_to_y_cluster(90.0, &centers), 0);
    }
}
