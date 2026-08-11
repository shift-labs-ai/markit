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

/// A row fragment: one or more adjacent boxes forming a single cell
/// candidate (word boxes separated by less than a cell gap).
struct Fragment<'a> {
    boxes: Vec<&'a TextBox>,
    left: f64,
    right: f64,
}

impl<'a> Fragment<'a> {
    fn text(&self) -> String {
        self.boxes
            .iter()
            .map(|tb| tb.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Cluster a row's boxes (sorted by left) into cell fragments: adjacent
/// boxes closer than the cell gap belong to the same cell.
fn row_fragments<'a>(row: &Row<'a>) -> Vec<Fragment<'a>> {
    let mut fragments: Vec<Fragment<'a>> = Vec::new();
    for tb in &row.boxes {
        match fragments.last_mut() {
            Some(fragment) if tb.bounds.left - fragment.right < MIN_CELL_GAP => {
                fragment.boxes.push(tb);
                fragment.right = fragment.right.max(tb.bounds.right);
            }
            _ => fragments.push(Fragment {
                boxes: vec![tb],
                left: tb.bounds.left,
                right: tb.bounds.right,
            }),
        }
    }
    fragments
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
    row.boxes.len() >= 2 && row_fragments(row).len() >= 2
}

/// Cluster the x-intervals of a run's cell fragments into columns.
/// Returns the (left, right) extents per column, or None when the
/// column count is implausible.
fn cluster_columns(rows: &[&Row]) -> Option<Vec<(f64, f64)>> {
    let mut intervals: Vec<(f64, f64)> = rows
        .iter()
        .flat_map(|row| {
            row_fragments(row)
                .into_iter()
                .map(|fragment| (fragment.left, fragment.right))
        })
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

fn column_of(columns: &[(f64, f64)], left: f64, right: f64) -> Option<usize> {
    let center = (left + right) / 2.0;
    columns
        .iter()
        .position(|(l, r)| center >= *l && center <= *r)
}

/// Maximum header rows absorbed above a detected table body.
const MAX_HEADER_ROWS: usize = 2;

/// Maximum vertical distance (in row heights) between a header row and
/// the table body.
const MAX_HEADER_GAP_ROWS: f64 = 3.0;

/// One header-row cell: (column, text, source boxes).
type HeaderCell<'a> = (usize, String, Vec<&'a TextBox>);

/// A built table body: column extents plus its cells.
type BuiltBody = (Vec<(f64, f64)>, Vec<TableCell>);

/// Absorb up to MAX_HEADER_ROWS rows immediately above the run as
/// header rows. A header row's fragments must map to ≥2 distinct
/// columns — a single wide fragment is a caption, not a header.
fn absorb_header_rows<'a>(
    rows: &[Row<'a>],
    run_start: usize,
    columns: &[(f64, f64)],
    consumed_so_far: &[String],
) -> Vec<Vec<HeaderCell<'a>>> {
    let consumed: std::collections::HashSet<&str> =
        consumed_so_far.iter().map(|s| s.as_str()).collect();
    let mut headers = Vec::new();
    let mut below = run_start;
    for _ in 0..MAX_HEADER_ROWS {
        if below == 0 {
            break;
        }
        let candidate = &rows[below - 1];
        if candidate
            .boxes
            .iter()
            .any(|tb| consumed.contains(tb.id.as_str()))
        {
            break;
        }
        let below_row = &rows[below];
        let gap = candidate.bottom - below_row.top;
        let row_height = (candidate.top - candidate.bottom).max(1.0);
        if gap < 0.0 || gap > MAX_HEADER_GAP_ROWS * row_height {
            break;
        }
        // Assign fragments to columns by center; require ≥2 distinct.
        let mut cells: Vec<HeaderCell> = Vec::new();
        let mut cols_hit: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for fragment in row_fragments(candidate) {
            let center = (fragment.left + fragment.right) / 2.0;
            let column = columns
                .iter()
                .position(|(l, r)| center >= *l && center <= *r)
                .unwrap_or_else(|| {
                    // Nearest column by center distance.
                    let mut best = 0usize;
                    let mut best_d = f64::INFINITY;
                    for (ci, (l, r)) in columns.iter().enumerate() {
                        let c = (l + r) / 2.0;
                        let d = (c - center).abs();
                        if d < best_d {
                            best_d = d;
                            best = ci;
                        }
                    }
                    best
                });
            cols_hit.insert(column);
            cells.push((column, fragment.text(), fragment.boxes.clone()));
        }
        if cols_hit.len() < 2 {
            break;
        }
        headers.push(cells);
        below -= 1;
    }
    headers.reverse();
    headers
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
        if j - i < MIN_ROWS {
            i += 1;
            continue;
        }

        // Grouped header rows (multi-column spans, sub-labels) straddle
        // the body's columns and would abort the whole candidate. Retry
        // with leading rows dropped; the dropped rows come back through
        // header absorption below.
        let mut start = i;
        let mut built: Option<BuiltBody> = None;
        while j - start >= MIN_ROWS {
            let run: Vec<&Row> = rows[start..j].iter().collect();
            let Some(columns) = cluster_columns(&run) else {
                start += 1;
                continue;
            };
            // A grouped header (a span across several body columns) fuses
            // columns during clustering. If dropping the leading row
            // yields a finer column structure, the leading row is a
            // header — drop it here, absorb it below.
            if j - (start + 1) >= MIN_ROWS {
                let without_first: Vec<&Row> = rows[start + 1..j].iter().collect();
                if let Some(finer) = cluster_columns(&without_first) {
                    if finer.len() > columns.len() {
                        start += 1;
                        continue;
                    }
                }
            }
            let mut cells: Vec<TableCell> = Vec::new();
            let mut filled = 0usize;
            let mut ok = true;
            'rows: for (row_index, row) in run.iter().enumerate() {
                let mut row_cells: Vec<Option<String>> = vec![None; columns.len()];
                for fragment in row_fragments(row) {
                    let Some(column) = column_of(&columns, fragment.left, fragment.right) else {
                        ok = false;
                        break 'rows;
                    };
                    let text = fragment.text();
                    match &mut row_cells[column] {
                        Some(existing) => {
                            existing.push(' ');
                            existing.push_str(&text);
                        }
                        slot @ None => {
                            *slot = Some(text);
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
            let total = (j - start) * columns.len();
            if ok && (filled as f64) >= MIN_FILL_RATIO * total as f64 {
                built = Some((columns, cells));
                break;
            }
            start += 1;
        }
        let Some((columns, mut cells)) = built else {
            i = j.max(i + 1);
            continue;
        };
        let run: Vec<&Row> = rows[start..j].iter().collect();

        // Header rows above the body join the grid so column-heading
        // relations survive.
        let headers = absorb_header_rows(&rows, start, &columns, &consumed);
        let header_count = headers.len();
        if header_count > 0 {
            for cell in cells.iter_mut() {
                cell.row += header_count;
            }
            for (row_index, header_cells) in headers.iter().enumerate() {
                let mut texts: Vec<String> = vec![String::new(); columns.len()];
                for (column, text, boxes) in header_cells {
                    if texts[*column].is_empty() {
                        texts[*column] = text.clone();
                    } else {
                        texts[*column] = format!("{} {}", texts[*column], text);
                    }
                    for tb in boxes {
                        consumed.push(tb.id.clone());
                    }
                }
                for (column, text) in texts.into_iter().enumerate() {
                    cells.push(TableCell {
                        row: row_index,
                        col: column,
                        text,
                        row_span: 1,
                        col_span: 1,
                    });
                }
            }
        }

        let top_y = run
            .iter()
            .flat_map(|row| row.boxes.iter())
            .map(|tb| tb.bounds.top)
            .fold(f64::NEG_INFINITY, f64::max);
        grids.push(TableGrid {
            page_number,
            rows: run.len() + header_count,
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
    fn multi_word_cells_cluster_into_one_column() {
        // "TAM 107" split into two boxes 5pt apart must still read as
        // one cell; the 120pt gap to the value column is the divider.
        let mut boxes = Vec::new();
        for r in 0..3 {
            let y = 700.0 - r as f64 * 15.0;
            boxes.push(tb(&format!("a{r}"), "TAM", 72.0, y, 30.0));
            boxes.push(tb(&format!("b{r}"), "107", 107.0, y, 25.0));
            boxes.push(tb(&format!("c{r}"), "13.5", 252.0, y, 30.0));
        }
        let (grids, _) = detect_borderless_tables(&boxes, 1);
        assert_eq!(grids.len(), 1);
        let grid = &grids[0];
        assert_eq!(grid.cols, 2);
        let cell = |r: usize, c: usize| -> &str {
            &grid
                .cells
                .iter()
                .find(|cell| cell.row == r && cell.col == c)
                .unwrap()
                .text
        };
        assert_eq!(cell(0, 0), "TAM 107");
        assert_eq!(cell(1, 1), "13.5");
    }

    #[test]
    fn grouped_header_row_is_absorbed_into_the_grid() {
        // A header row whose fragments straddle column groups sits just
        // above the body; it must become row 0 of the table. The wide
        // caption above it must NOT be absorbed.
        let mut boxes = vec![
            tb(
                "cap",
                "Table 4. Results of the control group.",
                72.0,
                725.0,
                300.0,
            ),
            tb("h0", "Microbiota", 72.0, 706.0, 70.0),
            tb("h1", "Pre-Test", 192.0, 706.0, 60.0),
            tb("h2", "Post-Test", 312.0, 706.0, 60.0),
        ];
        let body = [
            ["Bifidobacterium", "4.79", "4.81"],
            ["Bacteroides", "0.00", "3.03"],
            ["Clostridium", "6.73", "6.59"],
        ];
        for (r, row) in body.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                boxes.push(tb(
                    &format!("r{r}c{c}"),
                    cell,
                    72.0 + c as f64 * 120.0,
                    690.0 - r as f64 * 15.0,
                    60.0,
                ));
            }
        }
        let (grids, consumed) = detect_borderless_tables(&boxes, 1);
        assert_eq!(grids.len(), 1);
        let grid = &grids[0];
        assert_eq!((grid.rows, grid.cols), (4, 3));
        let cell = |r: usize, c: usize| -> &str {
            &grid
                .cells
                .iter()
                .find(|cell| cell.row == r && cell.col == c)
                .unwrap()
                .text
        };
        assert_eq!(cell(0, 0), "Microbiota");
        assert_eq!(cell(0, 1), "Pre-Test");
        assert_eq!(cell(1, 0), "Bifidobacterium");
        assert!(consumed.contains(&"h1".to_string()));
        assert!(!consumed.contains(&"cap".to_string()), "caption absorbed");
    }

    #[test]
    fn straddling_group_header_is_dropped_from_run_and_absorbed() {
        // A grouped header ("Mean ± SD" spanning value columns) straddles
        // the body columns: it must not abort the candidate; it joins as
        // a header row mapped by center.
        let mut boxes = vec![
            tb("g0", "Species", 72.0, 706.0, 60.0),
            // Spans across both value columns (192..372).
            tb("g1", "Mean \u{b1} SD", 210.0, 706.0, 140.0),
        ];
        let body = [
            ["Bifidobacterium", "4.79", "4.81"],
            ["Bacteroides", "0.00", "3.03"],
            ["Clostridium", "6.73", "6.59"],
        ];
        for (r, row) in body.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                boxes.push(tb(
                    &format!("r{r}c{c}"),
                    cell,
                    72.0 + c as f64 * 120.0,
                    690.0 - r as f64 * 15.0,
                    60.0,
                ));
            }
        }
        let (grids, consumed) = detect_borderless_tables(&boxes, 1);
        assert_eq!(grids.len(), 1);
        let grid = &grids[0];
        assert_eq!((grid.rows, grid.cols), (4, 3));
        let cell = |r: usize, c: usize| -> &str {
            &grid
                .cells
                .iter()
                .find(|cell| cell.row == r && cell.col == c)
                .unwrap()
                .text
        };
        assert_eq!(cell(0, 0), "Species");
        assert!(cell(0, 1).contains("Mean"), "got {:?}", cell(0, 1));
        assert_eq!(cell(1, 1), "4.79");
        assert!(consumed.contains(&"g1".to_string()));
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
