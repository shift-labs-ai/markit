//! Full-grid table construction: split cross-column text boxes,
//! allocate cells, place text by border ray confidence, and expand
//! clustered sub-rows. The page dispatcher receives only the completed
//! TableGrid and consumed text-box IDs.

use std::collections::{HashMap, HashSet};

use crate::converters::pdf::types::{Bounds, Segment, TableCell, TableGrid, TextBox};

use super::lines::expand_sub_rows_by_y_clusters;
use super::prune::prune_empty_rows_and_cols;
use super::raycast::RayIndex;

/// Find which column a horizontal position falls into.
/// Returns None if outside the grid.
fn find_col(x: f64, x_lines: &[f64]) -> Option<usize> {
    (0..x_lines.len().saturating_sub(1)).find(|&i| x >= x_lines[i] && x <= x_lines[i + 1])
}

/// When a text box spans across one or more vertical column boundaries,
/// split it into multiple virtual text boxes — one per column — with the
/// text divided proportionally by width.
///
/// We split at word boundaries closest to the proportional split point
/// so we don't chop words in half.
fn split_cross_column_boxes(text_boxes: &[TextBox], x_lines: &[f64]) -> Vec<TextBox> {
    let mut result: Vec<TextBox> = Vec::new();
    let margin = 5.0; // allow small overlap before considering it cross-column

    for tb in text_boxes {
        let left_col = find_col(tb.bounds.left + margin, x_lines);
        let right_col = find_col(tb.bounds.right - margin, x_lines);

        // Not spanning columns, or outside grid — keep as-is
        match (left_col, right_col) {
            (Some(lc), Some(rc)) if lc != rc => {
                // Text box spans from lc to rc — split it
                let total_width = tb.bounds.right - tb.bounds.left;
                if total_width <= 0.0 {
                    result.push(tb.clone());
                    continue;
                }

                let words: Vec<&str> = tb.text.split_whitespace().collect();
                if words.len() <= 1 {
                    // Single word spanning columns — just assign to whichever col has more overlap
                    result.push(tb.clone());
                    continue;
                }

                let mut word_at = 0usize;
                let mut current_left = tb.bounds.left;

                for col in lc..=rc {
                    if word_at == words.len() {
                        break;
                    }
                    let col_right = if col < x_lines.len() - 1 {
                        x_lines[col + 1]
                    } else {
                        tb.bounds.right
                    };
                    let segment_right = col_right.min(tb.bounds.right);

                    if col == rc {
                        // Last column — take all remaining words
                        result.push(TextBox {
                            id: format!("{}-split{}", tb.id, col),
                            text: words[word_at..].join(" "),
                            bounds: Bounds {
                                left: current_left,
                                right: tb.bounds.right,
                                ..tb.bounds
                            },
                            ..tb.clone()
                        });
                        word_at = words.len();
                    } else {
                        // Find how many words fit in this column segment proportionally
                        let segment_width = segment_right - current_left;
                        let fraction_of_total = segment_width / total_width;
                        // TS .length is UTF-16 code units, not bytes.
                        let approx_chars = (fraction_of_total
                            * tb.text.encode_utf16().count() as f64)
                            .round() as usize;

                        // Walk the unconsumed suffix to find the split
                        // closest to the proportional point.
                        let remaining_words = &words[word_at..];
                        let mut char_count: usize = 0;
                        let mut split_idx: usize = 0;
                        for (w, word) in remaining_words.iter().enumerate() {
                            let next_count = char_count
                                + word.encode_utf16().count()
                                + if w > 0 { 1 } else { 0 };
                            if next_count > approx_chars && split_idx > 0 {
                                break;
                            }
                            char_count = next_count;
                            split_idx = w + 1;
                        }

                        if split_idx == 0 {
                            split_idx = 1; // take at least one word
                        }
                        if split_idx >= remaining_words.len() {
                            // All remaining words fit here
                            result.push(TextBox {
                                id: format!("{}-split{}", tb.id, col),
                                text: remaining_words.join(" "),
                                bounds: Bounds {
                                    left: current_left,
                                    right: segment_right,
                                    ..tb.bounds
                                },
                                ..tb.clone()
                            });
                            word_at = words.len();
                        } else {
                            result.push(TextBox {
                                id: format!("{}-split{}", tb.id, col),
                                text: remaining_words[..split_idx].join(" "),
                                bounds: Bounds {
                                    left: current_left,
                                    right: segment_right,
                                    ..tb.bounds
                                },
                                ..tb.clone()
                            });
                            word_at += split_idx;
                            current_left = segment_right;
                        }
                    }
                }
            }
            _ => {
                result.push(tb.clone());
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Full grid table (H + V lines)
// ---------------------------------------------------------------------------

fn build_cells(rows: usize, cols: usize) -> Vec<TableCell> {
    let mut cells = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            cells.push(TableCell {
                row,
                col,
                text: String::new(),
                row_span: 1,
                col_span: 1,
            });
        }
    }
    cells
}

pub(super) fn build_table_grid(
    page_number: u32,
    y_lines: &[f64],
    x_lines: &[f64],
    filtered_segments: &[Segment],
    text_boxes: &[TextBox],
) -> (TableGrid, Vec<String>) {
    let mut rows = y_lines.len() - 1;
    let cols = x_lines.len() - 1;
    let mut cells = build_cells(rows, cols);
    let mut consumed_ids: Vec<String> = Vec::new();

    let y_min = y_lines[y_lines.len() - 1];
    let y_max = y_lines[0];
    let x_min = x_lines[0];
    let x_max = x_lines[x_lines.len() - 1];

    // Split text boxes that span multiple columns before placement
    let split_boxes = split_cross_column_boxes(text_boxes, x_lines);
    let ray_index = RayIndex::new(filtered_segments);

    // Track which split piece IDs get placed in cells
    let mut placed_split_ids: HashSet<String> = HashSet::new();

    // Look for header text boxes just above the grid.
    // Use the ORIGINAL (unsplit) text boxes for header detection so that
    // wide paragraph text isn't falsely split into column-sized header chunks.
    // Reject boxes wider than 1.5 columns — those are paragraph text, not headers.
    let avg_col_width = (x_max - x_min) / cols as f64;
    let max_header_box_width = avg_col_width * 1.5;
    let header_boxes: Vec<&TextBox> = text_boxes
        .iter()
        .filter(|tb| {
            let cy = (tb.bounds.top + tb.bounds.bottom) / 2.0;
            let cx = (tb.bounds.left + tb.bounds.right) / 2.0;
            let box_width = tb.bounds.right - tb.bounds.left;
            cy > y_max
                && cy <= y_max + 20.0
                && cx >= x_min
                && cx <= x_max
                && box_width <= max_header_box_width
        })
        .collect();

    if !header_boxes.is_empty() {
        rows += 1;
        for cell in cells.iter_mut() {
            cell.row += 1;
        }
        for col in 0..cols {
            cells.push(TableCell {
                row: 0,
                col,
                text: String::new(),
                row_span: 1,
                col_span: 1,
            });
        }
        for tb in &header_boxes {
            let cx = (tb.bounds.left + tb.bounds.right) / 2.0;
            let col = x_lines.windows(2).position(|w| cx >= w[0] && cx <= w[1]);
            if let Some(col) = col {
                if col < cols {
                    if let Some(cell) = cells.iter_mut().find(|c| c.row == 0 && c.col == col) {
                        if cell.text.is_empty() {
                            cell.text = tb.text.clone();
                        } else {
                            cell.text = format!("{} {}", cell.text, tb.text);
                        }
                        consumed_ids.push(tb.id.clone());
                    }
                }
            }
        }
    }

    // Cell positions are stable until sub-row expansion; index them
    // once instead of scanning every cell for every text box.
    let cell_index: HashMap<(usize, usize), usize> = cells
        .iter()
        .enumerate()
        .map(|(i, c)| ((c.row, c.col), i))
        .collect();
    // cell_boxes: cell_index -> indices into split_boxes
    let mut cell_boxes: HashMap<usize, Vec<usize>> = HashMap::new();

    for (box_idx, tb) in split_boxes.iter().enumerate() {
        let cx = (tb.bounds.left + tb.bounds.right) / 2.0;
        let cy = (tb.bounds.top + tb.bounds.bottom) / 2.0;

        if cy < y_min || cy > y_max || cx < x_min || cx > x_max {
            continue;
        }

        let row_opt = y_lines.windows(2).position(|w| cy <= w[0] && cy >= w[1]);
        let mut row = match row_opt {
            Some(r) => r,
            None => continue,
        };

        let max_row = if !header_boxes.is_empty() {
            rows - 1
        } else {
            rows
        };
        if row >= max_row {
            continue;
        }
        if !header_boxes.is_empty() {
            row += 1;
        }

        let col = match x_lines.windows(2).position(|w| cx >= w[0] && cx <= w[1]) {
            Some(c) => c,
            None => continue,
        };
        if col >= cols {
            continue;
        }
        if !ray_index.any_hit(tb) {
            continue;
        }

        let Some(&cell_idx) = cell_index.get(&(row, col)) else {
            continue;
        };

        cell_boxes.entry(cell_idx).or_default().push(box_idx);
        consumed_ids.push(tb.id.clone());
        if tb.id.contains("-split") {
            placed_split_ids.insert(tb.id.clone());
        }
    }

    rows = expand_sub_rows_by_y_clusters(rows, cols, &mut cells, &mut cell_boxes, &split_boxes);

    // Merge text boxes within each cell into cell text
    for (&cell_idx, box_indices) in &cell_boxes {
        let mut boxes: Vec<&TextBox> = box_indices.iter().map(|&bi| &split_boxes[bi]).collect();
        boxes.sort_by(|a, b| {
            b.bounds
                .top
                .partial_cmp(&a.bounds.top)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut lines: Vec<String> = Vec::new();
        let mut current_line: Vec<String> = Vec::new();
        let mut current_y = boxes[0].bounds.top;

        for bx in &boxes {
            if (bx.bounds.top - current_y).abs() > 5.0 {
                lines.push(current_line.join(" "));
                current_line = vec![bx.text.clone()];
                current_y = bx.bounds.top;
            } else {
                current_line.push(bx.text.clone());
            }
        }
        if !current_line.is_empty() {
            lines.push(current_line.join(" "));
        }
        cells[cell_idx].text = lines.join("<br>");
    }

    let grid = prune_empty_rows_and_cols(TableGrid {
        page_number,
        rows,
        cols,
        cells,
        warnings: vec![],
        top_y: y_lines[0],
        is_borderless: false,
    });

    // Also consume the original (unsplit) text box IDs when any of their
    // split pieces were placed in a cell. A set avoids quadratic Vec
    // membership scans on dense pages.
    let mut consumed_seen: HashSet<String> = consumed_ids.iter().cloned().collect();
    for split_id in &placed_split_ids {
        let orig_id = split_id
            .split("-split")
            .next()
            .unwrap_or(split_id)
            .to_string();
        if consumed_seen.insert(orig_id.clone()) {
            consumed_ids.push(orig_id);
        }
    }

    (grid, consumed_ids)
}

// ---------------------------------------------------------------------------
// H-line-only table (inferred columns)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str, left: f64, right: f64) -> TextBox {
        TextBox {
            id: "t".into(),
            text: text.into(),
            page_number: 1,
            font_size: 10.0,
            is_bold: false,
            bounds: Bounds {
                left,
                right,
                bottom: 10.0,
                top: 20.0,
            },
        }
    }

    #[test]
    fn column_lookup_includes_outer_boundaries() {
        let x = [0.0, 100.0, 200.0];
        assert_eq!(find_col(0.0, &x), Some(0));
        assert_eq!(find_col(100.0, &x), Some(0));
        assert_eq!(find_col(200.0, &x), Some(1));
        assert_eq!(find_col(201.0, &x), None);
    }

    #[test]
    fn multiword_cross_column_box_splits_by_word_position() {
        let source = text("left right", 20.0, 180.0);
        let split = split_cross_column_boxes(&[source], &[0.0, 100.0, 200.0]);
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].text, "left");
        assert_eq!(split[1].text, "right");
        assert_eq!(split[0].id, "t-split0");
        assert_eq!(split[1].id, "t-split1");
    }

    #[test]
    fn single_word_cross_column_box_stays_whole() {
        let source = text("unsplittable", 20.0, 180.0);
        assert_eq!(
            split_cross_column_boxes(std::slice::from_ref(&source), &[0.0, 100.0, 200.0]),
            [source]
        );
    }
}
