//! Borderless table detection.
//!
//! Ruled tables are recovered from vector segments (`grid.rs`). Tables
//! drawn without any rules — column headers over aligned value columns —
//! leave nothing for segment detection, so they are reconstructed from
//! text geometry alone:
//!
//!   1. Group text boxes into visual rows by Y midpoint.
//!   2. A row is tabular when it has ≥2 fragments, every neighbor pair
//!      separated by a clear cell gap.
//!   3. Runs of ≥3 vertically adjacent tabular rows form a candidate.
//!   4. Fragment x-intervals across the run cluster into columns; the
//!      candidate is accepted when the rows fill the grid densely and
//!      no fragment straddles two columns.
//!
//! Conservative by design: a false table mangles prose with pipe noise,
//! so failed candidates are left as free text.

use crate::converters::pdf::types::{TableCell, TableGrid, TextBox};

/// Y midpoint tolerance for grouping boxes onto one visual row.
const ROW_Y_TOLERANCE: f64 = 3.0;

/// Minimum horizontal gap between neighboring fragments for a row to
/// read as cells rather than words of a sentence.
const MIN_CELL_GAP: f64 = 15.0;

/// Minimum number of consecutive tabular rows.
const MIN_ROWS: usize = 3;

/// Maximum vertical gap between consecutive rows, in multiples of the
/// taller row's height.
const MAX_ROW_GAP_RATIO: f64 = 2.5;

/// Column count sanity bounds.
const MIN_COLS: usize = 2;
const MAX_COLS: usize = 12;

/// Minimum fraction of grid cells that must be filled.
const MIN_FILL_RATIO: f64 = 0.55;

struct Row<'a> {
    boxes: Vec<&'a TextBox>,
    top: f64,
    bottom: f64,
}

fn group_rows(text_boxes: &[TextBox]) -> Vec<Row<'_>> {
    let mut sorted: Vec<&TextBox> = text_boxes.iter().collect();
    sorted.sort_by(|a, b| {
        let ya = (a.bounds.top + a.bounds.bottom) / 2.0;
        let yb = (b.bounds.top + b.bounds.bottom) / 2.0;
        yb.total_cmp(&ya)
    });
    let mut rows: Vec<Row> = Vec::new();
    for tb in sorted {
        let mid = (tb.bounds.top + tb.bounds.bottom) / 2.0;
        match rows.last_mut() {
            Some(row) => {
                let row_mid = (row.top + row.bottom) / 2.0;
                if (row_mid - mid).abs() <= ROW_Y_TOLERANCE {
                    row.boxes.push(tb);
                    row.top = row.top.max(tb.bounds.top);
                    row.bottom = row.bottom.min(tb.bounds.bottom);
                } else {
                    rows.push(Row {
                        boxes: vec![tb],
                        top: tb.bounds.top,
                        bottom: tb.bounds.bottom,
                    });
                }
            }
            None => rows.push(Row {
                boxes: vec![tb],
                top: tb.bounds.top,
                bottom: tb.bounds.bottom,
            }),
        }
    }
    for row in rows.iter_mut() {
        row.boxes
            .sort_by(|a, b| a.bounds.left.total_cmp(&b.bounds.left));
    }
    rows
}

fn row_is_tabular(row: &Row) -> bool {
    if row.boxes.len() < 2 {
        return false;
    }
    row.boxes
        .windows(2)
        .all(|pair| pair[1].bounds.left - pair[0].bounds.right >= MIN_CELL_GAP)
}

/// Cluster the x-intervals of a run's fragments into columns. Returns
/// the (left, right) extents per column, or None when a fragment would
/// straddle two clusters.
fn cluster_columns(rows: &[&Row]) -> Option<Vec<(f64, f64)>> {
    let mut intervals: Vec<(f64, f64)> = rows
        .iter()
        .flat_map(|row| row.boxes.iter().map(|tb| (tb.bounds.left, tb.bounds.right)))
        .collect();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut columns: Vec<(f64, f64)> = Vec::new();
    for (left, right) in intervals {
        match columns.last_mut() {
            Some(column) if left <= column.1 + MIN_CELL_GAP / 2.0 => {
                column.1 = column.1.max(right);
            }
            _ => columns.push((left, right)),
        }
    }
    if columns.len() < MIN_COLS || columns.len() > MAX_COLS {
        return None;
    }
    Some(columns)
}

fn column_of(columns: &[(f64, f64)], tb: &TextBox) -> Option<usize> {
    let center = (tb.bounds.left + tb.bounds.right) / 2.0;
    columns
        .iter()
        .position(|(left, right)| center >= *left && center <= *right)
}

/// Detect borderless tables among free text boxes. Returns the grids
/// plus the ids of every consumed text box.
pub fn detect_borderless_tables(
    text_boxes: &[TextBox],
    page_number: u32,
) -> (Vec<TableGrid>, Vec<String>) {
    let rows = group_rows(text_boxes);
    let mut grids = Vec::new();
    let mut consumed = Vec::new();

    let mut i = 0;
    while i < rows.len() {
        if !row_is_tabular(&rows[i]) {
            i += 1;
            continue;
        }
        // Extend the run over vertically adjacent tabular rows.
        let mut j = i + 1;
        while j < rows.len() && row_is_tabular(&rows[j]) {
            let prev = &rows[j - 1];
            let cur = &rows[j];
            let gap = prev.bottom - cur.top;
            let row_height = (prev.top - prev.bottom).max(cur.top - cur.bottom).max(1.0);
            if gap < 0.0 || gap > MAX_ROW_GAP_RATIO * row_height {
                break;
            }
            j += 1;
        }
        let run: Vec<&Row> = rows[i..j].iter().collect();
        if run.len() < MIN_ROWS {
            i += 1;
            continue;
        }

        let Some(columns) = cluster_columns(&run) else {
            i = j;
            continue;
        };

        // Assign fragments to columns; abort on any straddler.
        let mut cells: Vec<TableCell> = Vec::new();
        let mut filled = 0usize;
        let mut ok = true;
        'rows: for (row_index, row) in run.iter().enumerate() {
            let mut row_cells: Vec<Option<String>> = vec![None; columns.len()];
            for tb in &row.boxes {
                let Some(column) = column_of(&columns, tb) else {
                    ok = false;
                    break 'rows;
                };
                match &mut row_cells[column] {
                    Some(text) => {
                        text.push(' ');
                        text.push_str(&tb.text);
                    }
                    slot @ None => {
                        *slot = Some(tb.text.clone());
                        filled += 1;
                    }
                }
            }
            for (column, text) in row_cells.into_iter().enumerate() {
                cells.push(TableCell {
                    row: row_index,
                    col: column,
                    text: text.unwrap_or_default(),
                    row_span: 1,
                    col_span: 1,
                });
            }
        }
        let total = run.len() * columns.len();
        if !ok || (filled as f64) < MIN_FILL_RATIO * total as f64 {
            i = j.max(i + 1);
            continue;
        }

        let top_y = run
            .iter()
            .flat_map(|row| row.boxes.iter())
            .map(|tb| tb.bounds.top)
            .fold(f64::NEG_INFINITY, f64::max);
        grids.push(TableGrid {
            page_number,
            rows: run.len(),
            cols: columns.len(),
            cells,
            warnings: Vec::new(),
            top_y,
            is_borderless: true,
        });
        for row in &run {
            for tb in &row.boxes {
                consumed.push(tb.id.clone());
            }
        }
        i = j;
    }

    (grids, consumed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converters::pdf::types::Bounds;

    fn tb(id: &str, text: &str, x: f64, y: f64, w: f64) -> TextBox {
        TextBox {
            id: id.to_string(),
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

    fn aligned_table() -> Vec<TextBox> {
        let mut boxes = Vec::new();
        let headers = ["Name", "Role", "Age"];
        let rows = [
            ["Alice", "CEO", "44"],
            ["Bob", "CTO", "41"],
            ["Carol", "CFO", "39"],
        ];
        for (c, h) in headers.iter().enumerate() {
            boxes.push(tb(
                &format!("h{c}"),
                h,
                72.0 + c as f64 * 120.0,
                700.0,
                60.0,
            ));
        }
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                boxes.push(tb(
                    &format!("r{r}c{c}"),
                    cell,
                    72.0 + c as f64 * 120.0,
                    685.0 - r as f64 * 15.0,
                    60.0,
                ));
            }
        }
        boxes
    }

    #[test]
    fn detects_an_aligned_borderless_table() {
        let boxes = aligned_table();
        let (grids, consumed) = detect_borderless_tables(&boxes, 1);
        assert_eq!(grids.len(), 1);
        let grid = &grids[0];
        assert_eq!((grid.rows, grid.cols), (4, 3));
        assert!(grid.is_borderless);
        assert_eq!(consumed.len(), boxes.len());
        let cell = |r: usize, c: usize| -> &str {
            &grid
                .cells
                .iter()
                .find(|cell| cell.row == r && cell.col == c)
                .unwrap()
                .text
        };
        assert_eq!(cell(0, 0), "Name");
        assert_eq!(cell(2, 1), "CTO");
        assert_eq!(cell(3, 2), "39");
    }

    #[test]
    fn prose_lines_are_not_a_table() {
        // Single full-width fragments per line: no cell gaps, no table.
        let boxes: Vec<TextBox> = (0..5)
            .map(|i| {
                tb(
                    &format!("p{i}"),
                    "A full sentence of body prose text.",
                    72.0,
                    700.0 - i as f64 * 15.0,
                    400.0,
                )
            })
            .collect();
        let (grids, consumed) = detect_borderless_tables(&boxes, 1);
        assert!(grids.is_empty());
        assert!(consumed.is_empty());
    }

    #[test]
    fn two_tabular_rows_are_not_enough() {
        let boxes = vec![
            tb("a0", "Name", 72.0, 700.0, 60.0),
            tb("a1", "Role", 200.0, 700.0, 60.0),
            tb("b0", "Alice", 72.0, 685.0, 60.0),
            tb("b1", "CEO", 200.0, 685.0, 60.0),
        ];
        let (grids, _) = detect_borderless_tables(&boxes, 1);
        assert!(grids.is_empty());
    }

    #[test]
    fn distant_tabular_regions_do_not_join() {
        let mut boxes = aligned_table();
        // A second aligned pair far below: too far to join the run, too
        // short to stand alone.
        boxes.push(tb("x0", "Key", 72.0, 300.0, 60.0));
        boxes.push(tb("x1", "Value", 200.0, 300.0, 60.0));
        let (grids, _) = detect_borderless_tables(&boxes, 1);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].rows, 4);
    }
}
