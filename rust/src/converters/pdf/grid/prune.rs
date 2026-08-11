//! Remove structurally empty rows and columns from a completed table
//! while preserving cell spans and row/column order.

use std::collections::{HashMap, HashSet};

use crate::converters::pdf::types::{TableCell, TableGrid};

pub(super) fn prune_empty_rows_and_cols(table: TableGrid) -> TableGrid {
    let occupied_rows: HashSet<usize> = table
        .cells
        .iter()
        .filter(|c| !c.text.trim().is_empty())
        .map(|c| c.row)
        .collect();
    let occupied_cols: HashSet<usize> = table
        .cells
        .iter()
        .filter(|c| !c.text.trim().is_empty())
        .map(|c| c.col)
        .collect();
    if occupied_rows.is_empty() {
        return table;
    }

    let mut row_map: HashMap<usize, usize> = HashMap::new();
    let mut new_row: usize = 0;
    for r in 0..table.rows {
        if occupied_rows.contains(&r) {
            row_map.insert(r, new_row);
            new_row += 1;
        }
    }
    let mut col_map: HashMap<usize, usize> = HashMap::new();
    let mut new_col: usize = 0;
    for c in 0..table.cols {
        if occupied_cols.contains(&c) {
            col_map.insert(c, new_col);
            new_col += 1;
        }
    }

    let pruned_cells: Vec<TableCell> = table
        .cells
        .iter()
        .filter(|c| occupied_rows.contains(&c.row) && occupied_cols.contains(&c.col))
        .map(|c| TableCell {
            row: *row_map.get(&c.row).unwrap_or(&c.row),
            col: *col_map.get(&c.col).unwrap_or(&c.col),
            ..c.clone()
        })
        .collect();

    TableGrid {
        rows: new_row,
        cols: new_col,
        cells: pruned_cells,
        ..table
    }
}

// ---------------------------------------------------------------------------
// Diagram vs table discrimination
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(row: usize, col: usize, text: &str) -> TableCell {
        TableCell {
            row,
            col,
            text: text.into(),
            row_span: 1,
            col_span: 1,
        }
    }

    #[test]
    fn removes_empty_outer_rows_and_columns_and_reindexes() {
        let grid = TableGrid {
            page_number: 1,
            rows: 3,
            cols: 3,
            warnings: Vec::new(),
            top_y: 0.0,
            is_borderless: false,
            cells: vec![
                cell(0, 0, ""),
                cell(0, 1, ""),
                cell(0, 2, ""),
                cell(1, 0, ""),
                cell(1, 1, "kept"),
                cell(1, 2, ""),
                cell(2, 0, ""),
                cell(2, 1, ""),
                cell(2, 2, ""),
            ],
        };
        let pruned = prune_empty_rows_and_cols(grid);
        assert_eq!((pruned.rows, pruned.cols), (1, 1));
        assert_eq!(pruned.cells, [cell(0, 0, "kept")]);
    }

    #[test]
    fn all_empty_grid_remains_structurally_intact() {
        let grid = TableGrid {
            page_number: 1,
            rows: 1,
            cols: 1,
            warnings: Vec::new(),
            top_y: 0.0,
            is_borderless: false,
            cells: vec![cell(0, 0, "")],
        };
        let pruned = prune_empty_rows_and_cols(grid);
        assert_eq!((pruned.rows, pruned.cols), (1, 1));
        assert_eq!(pruned.cells, [cell(0, 0, "")]);
    }
}
