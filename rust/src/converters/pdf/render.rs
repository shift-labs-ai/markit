//! Markdown rendering for PDF pages.
//!
//! Converts table grids and free text boxes into markdown, handling:
//! - Table grid → markdown table (`| col | col |`)
//! - Free text → paragraphs with heading detection (by font size)
//! - Content ordering (top-to-bottom via Y coordinate)
//! - Paragraph wrap merging (lines broken across PDF line boundaries)
//! - Page number removal
//!
//! Ported from `@oharato/pdf2md-ts`, stripped of CJK/TDnet-specific logic.

use crate::converters::pdf::types::*;

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Convert full-width ASCII characters (Ａ→A, ！→! etc.) to normal ASCII.
fn normalize_full_width_ascii(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ('\u{FF01}'..='\u{FF5E}').contains(&ch) {
                char::from_u32(ch as u32 - 0xFEE0).unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect()
}

fn escape_pipes(text: &str) -> String {
    normalize_full_width_ascii(text)
        .replace('|', "\\|")
        .replace('\n', "<br>")
}

/// Parse a markdown pipe-delimited row into cell strings.
fn parse_pipe_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return vec![];
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    inner
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Table rendering
// ---------------------------------------------------------------------------

/// Render a TableGrid as a markdown table.
pub fn render_table_to_markdown(table: &TableGrid) -> String {
    if table.rows == 0 || table.cols == 0 {
        return String::new();
    }

    let mut matrix: Vec<Vec<String>> = (0..table.rows)
        .map(|_| vec![String::new(); table.cols])
        .collect();

    for cell in &table.cells {
        if cell.row < table.rows && cell.col < table.cols {
            matrix[cell.row][cell.col] = escape_pipes(cell.text.trim());
        }
    }

    let normalized = normalize_shifted_sparse_columns(matrix);
    let promoted = promote_sub_header_prefixes(normalized);

    let header = format!("| {} |", promoted[0].join(" | "));
    let divider = format!("| {} |", vec!["---"; promoted[0].len()].join(" | "));
    let body: String = promoted[1..]
        .iter()
        .map(|row| format!("| {} |", row.join(" | ")))
        .collect::<Vec<_>>()
        .join("\n");

    [header, divider, body]
        .iter()
        .filter(|l| !l.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fix tables with ≥5 columns where sparse single-value columns are
/// misaligned. Shifts those values to the adjacent dense column and
/// removes the now-empty sparse columns.
fn normalize_shifted_sparse_columns(matrix: Vec<Vec<String>>) -> Vec<Vec<String>> {
    if matrix.is_empty() || matrix[0].len() < 5 {
        return matrix;
    }

    let _rows = matrix.len();
    let cols = matrix[0].len();

    let counts: Vec<usize> = (0..cols)
        .map(|c| {
            matrix
                .iter()
                .filter(|row| !row[c].trim().is_empty())
                .count()
        })
        .collect();

    let dense_cols: std::collections::HashSet<usize> = counts
        .iter()
        .enumerate()
        .filter(|&(col, &count)| col == 0 || count >= 2)
        .map(|(col, _)| col)
        .collect();

    let sparse_cols: Vec<usize> = counts
        .iter()
        .enumerate()
        .filter(|&(col, &count)| col > 0 && col < cols - 1 && count == 1)
        .map(|(col, _)| col)
        .collect();

    if sparse_cols.len() < 2 || dense_cols.len() < 4 {
        return matrix;
    }

    let mut moves: Vec<(usize, usize, usize)> = Vec::new(); // (from, to, row)
    for &from in &sparse_cols {
        let row = matrix.iter().position(|r| !r[from].trim().is_empty());
        let to = from + 1;
        let row = match row {
            Some(r) => r,
            None => return matrix,
        };
        if !dense_cols.contains(&to) {
            return matrix;
        }
        if !matrix[row][to].trim().is_empty() {
            return matrix;
        }
        moves.push((from, to, row));
    }

    let mut copy: Vec<Vec<String>> = matrix.to_vec();
    for &(from, to, row) in &moves {
        if !copy[row][to].trim().is_empty() {
            copy[row][to] = format!("{} {}", copy[row][to], copy[row][from]);
        } else {
            copy[row][to] = copy[row][from].clone();
        }
        copy[row][from] = String::new();
    }

    let keep_cols: Vec<usize> = (0..cols)
        .filter(|&c| copy.iter().any(|row| !row[c].trim().is_empty()))
        .collect();

    if keep_cols.len() == cols {
        return copy;
    }

    copy.iter()
        .map(|row| keep_cols.iter().map(|&c| row[c].clone()).collect())
        .collect()
}

/// When a data row has ≥2 parenthesized qualifiers in non-first columns
/// (and the first column is empty), promote them into the header row.
fn promote_sub_header_prefixes(matrix: Vec<Vec<String>>) -> Vec<Vec<String>> {
    if matrix.len() < 2 {
        return matrix;
    }

    static PAREN_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^\([^)]{1,40}\)$").unwrap());
    let paren_re = &*PAREN_RE;
    let mut result: Vec<Vec<String>> = matrix.to_vec();
    let cols = matrix[0].len();
    let mut rows_to_remove: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for r in 1..result.len() {
        if rows_to_remove.contains(&r) {
            continue;
        }

        struct Promotable {
            col: usize,
            prefix: String,
            is_full_cell: bool,
        }

        let mut promotable: Vec<Promotable> = Vec::new();

        for col in 1..cols {
            let cell = result[r]
                .get(col)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if cell.is_empty() {
                continue;
            }

            let parts: Vec<&str> = cell.split("<br>").collect();
            if parts.len() == 1 && paren_re.is_match(&cell) {
                promotable.push(Promotable {
                    col,
                    prefix: cell.clone(),
                    is_full_cell: true,
                });
            } else if parts.len() >= 2 && paren_re.is_match(parts[0].trim()) {
                promotable.push(Promotable {
                    col,
                    prefix: parts[0].trim().to_string(),
                    is_full_cell: false,
                });
            }
        }

        if promotable.len() < 2 {
            continue;
        }
        if promotable.iter().any(|p| p.is_full_cell) && !result[r][0].trim().is_empty() {
            continue;
        }

        for p in &promotable {
            let header = result[0][p.col].trim().to_string();
            result[0][p.col] = if !header.is_empty() {
                format!("{} {}", header, p.prefix)
            } else {
                p.prefix.clone()
            };
            if p.is_full_cell {
                result[r][p.col] = String::new();
            } else {
                let parts: Vec<&str> = result[r][p.col].split("<br>").collect();
                result[r][p.col] = parts[1..].join("<br>");
            }
        }

        if result[r].iter().all(|cell| cell.trim().is_empty()) {
            rows_to_remove.insert(r);
        }
    }

    result
        .into_iter()
        .enumerate()
        .filter(|(r, _)| !rows_to_remove.contains(r))
        .map(|(_, row)| row)
        .collect()
}

// ---------------------------------------------------------------------------
// Free text rendering
// ---------------------------------------------------------------------------

/// Y tolerance for grouping text boxes onto the same visual line.
const TEXT_LINE_Y_TOLERANCE: f64 = 3.0;

/// Minimum X gap between adjacent boxes to mark line as tabular.
const TABULAR_X_GAP: f64 = 30.0;

struct TextLine {
    text: String,
    top_y: f64,
    font_size: f64,
    is_bold: bool,
    is_tabular: bool,
}

/// Minimum font size (pts) to consider when computing the modal body font.
/// Tiny labels from diagrams, footnote markers, and superscripts are excluded
/// so they don't skew the modal toward small sizes.
const MIN_BODY_FONT_SIZE: f64 = 7.0;

/// Compute the most frequent font size among text boxes, ignoring very small
/// text that likely comes from diagrams, footnotes, or superscripts.
fn modal_font_size(text_boxes: &[TextBox]) -> f64 {
    // JS Map iterates in INSERTION order — on count ties the size that was
    // first encountered in text-box order wins, not the smallest.
    let mut counts: Vec<(i64, usize)> = Vec::new();
    for tb in text_boxes {
        let size = (tb.font_size * 10.0).round() / 10.0;
        if size < MIN_BODY_FONT_SIZE {
            continue;
        }
        let key = (size * 10.0).round() as i64;
        match counts.iter_mut().find(|(k, _)| *k == key) {
            Some((_, c)) => *c += 1,
            None => counts.push((key, 1)),
        }
    }
    let mut modal = 0.0;
    let mut max_count = 0;
    for &(key, count) in &counts {
        if count > max_count {
            max_count = count;
            modal = key as f64 / 10.0;
        }
    }
    modal
}

/// Group free text boxes into horizontal lines, sorted top-to-bottom.
fn group_free_text_into_lines(text_boxes: &[TextBox]) -> Vec<TextLine> {
    if text_boxes.is_empty() {
        return vec![];
    }

    let mut sorted: Vec<&TextBox> = text_boxes.iter().collect();
    // Sort: higher Y first (top of page), then left-to-right
    // TS: dy = yb - ya; if abs(dy) > tolerance return dy; else left-to-left
    // yb - ya > 0 means a comes first (higher midY)
    // Tolerance-band comparator (not a total order) — see js_stable_sort.
    super::js_stable_sort(&mut sorted, |a, b| {
        let ya = (a.bounds.top + a.bounds.bottom) / 2.0;
        let yb = (b.bounds.top + b.bounds.bottom) / 2.0;
        let dy = yb - ya;
        if dy.abs() > TEXT_LINE_Y_TOLERANCE {
            // TS returns dy (JS: negative ⇒ a first). dy < 0 ⇔ a higher on the
            // page ⇔ a sorts first — i.e. plain partial_cmp, NO reversal.
            dy.partial_cmp(&0.0f64).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.bounds
                .left
                .partial_cmp(&b.bounds.left)
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    });

    let mut lines: Vec<TextLine> = Vec::new();
    let mut cur_parts: Vec<String> = vec![sorted[0].text.clone()];
    let mut cur_boxes: Vec<&TextBox> = vec![sorted[0]];
    let mut cur_y = (sorted[0].bounds.top + sorted[0].bounds.bottom) / 2.0;
    let cur_top_y_init = cur_y;
    let mut cur_top_y = cur_top_y_init;
    let mut cur_font_size = sorted[0].font_size;
    let mut cur_is_bold = sorted[0].is_bold;

    let finish_line = |parts: &[String],
                       boxes: &[&TextBox],
                       top_y: f64,
                       font_size: f64,
                       is_bold: bool,
                       lines: &mut Vec<TextLine>| {
        let mut is_tabular = false;
        for j in 1..boxes.len() {
            if boxes[j].bounds.left - boxes[j - 1].bounds.right > TABULAR_X_GAP {
                is_tabular = true;
                break;
            }
        }
        lines.push(TextLine {
            text: parts.join(" "),
            top_y,
            font_size,
            is_bold,
            is_tabular,
        });
    };

    for i in 1..sorted.len() {
        let bx = sorted[i];
        let cy = (bx.bounds.top + bx.bounds.bottom) / 2.0;
        if (cy - cur_y).abs() <= TEXT_LINE_Y_TOLERANCE {
            cur_parts.push(bx.text.clone());
            cur_boxes.push(bx);
            cur_font_size = cur_font_size.max(bx.font_size);
            cur_is_bold = cur_is_bold || bx.is_bold;
        } else {
            finish_line(
                &cur_parts,
                &cur_boxes,
                cur_top_y,
                cur_font_size,
                cur_is_bold,
                &mut lines,
            );
            cur_parts = vec![bx.text.clone()];
            cur_boxes = vec![bx];
            cur_y = cy;
            cur_top_y = cy;
            cur_font_size = bx.font_size;
            cur_is_bold = bx.is_bold;
        }
    }
    finish_line(
        &cur_parts,
        &cur_boxes,
        cur_top_y,
        cur_font_size,
        cur_is_bold,
        &mut lines,
    );
    lines
}

/// Determine markdown heading prefix based on font size relative to body.
fn heading_prefix(font_size: f64, body_font_size: f64, is_bold: bool) -> &'static str {
    if body_font_size <= 0.0 {
        return "";
    }

    let ratio = font_size / body_font_size;

    // Large headings (>2x body size)
    if ratio >= 2.0 {
        return "# ";
    }
    // Medium headings (~1.5x body size)
    if ratio >= 1.4 {
        return "## ";
    }
    // Small headings (bold and slightly larger)
    if ratio >= 1.1 && is_bold {
        return "### ";
    }

    ""
}

// ---------------------------------------------------------------------------
// Block merging
// ---------------------------------------------------------------------------

/// Merge consecutive blocks with the same heading prefix (wrapped headings).
fn merge_consecutive_headings(blocks: Vec<ContentBlock>, body_fs: f64) -> Vec<ContentBlock> {
    if blocks.is_empty() {
        return vec![];
    }

    static HEADING_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^(#{1,6} )").unwrap());
    let heading_re = &*HEADING_RE;
    let max_gap = (body_fs * 3.0).max(30.0);
    let mut merged: Vec<ContentBlock> = Vec::new();
    let mut cur = blocks[0].clone();

    for i in 1..blocks.len() {
        let next = &blocks[i];
        let cur_match = heading_re
            .find(&cur.content)
            .map(|m| m.as_str().to_string());
        let next_match = heading_re
            .find(&next.content)
            .map(|m| m.as_str().to_string());
        let gap = cur.top_y - next.top_y;

        if let (Some(ref cp), Some(ref np)) = (&cur_match, &next_match) {
            if cp == np && gap <= max_gap {
                let suffix = &next.content[np.len()..];
                cur = ContentBlock {
                    top_y: cur.top_y,
                    content: format!("{} {}", cur.content, suffix),
                    is_tabular: cur.is_tabular || next.is_tabular,
                };
                continue;
            }
        }

        merged.push(cur);
        cur = next.clone();
    }
    merged.push(cur);
    merged
}

/// Merge consecutive plain-text blocks that are wrapped lines of the same paragraph.
fn merge_paragraph_wraps(blocks: Vec<ContentBlock>, body_fs: f64) -> Vec<ContentBlock> {
    if blocks.is_empty() || body_fs <= 0.0 {
        return blocks;
    }

    static HEADING_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^#{1,6} ").unwrap());
    let heading_re = &*HEADING_RE;
    static SENTENCE_END_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"[.!?…)\]]\s*$").unwrap());
    let sentence_end_re = &*SENTENCE_END_RE;
    let max_gap = body_fs * 2.0;
    let min_wrap_length = 25;

    let mut merged: Vec<ContentBlock> = Vec::new();

    struct Cur {
        top_y: f64,
        last_top_y: f64,
        content: String,
        is_tabular: bool,
    }

    let mut cur = Cur {
        top_y: blocks[0].top_y,
        last_top_y: blocks[0].top_y,
        content: blocks[0].content.clone(),
        is_tabular: blocks[0].is_tabular,
    };

    for i in 1..blocks.len() {
        let next = &blocks[i];
        let cur_is_body = !heading_re.is_match(&cur.content) && !cur.content.starts_with('|');
        let next_is_body = !heading_re.is_match(&next.content) && !next.content.starts_with('|');
        let gap = cur.last_top_y - next.top_y;

        let is_wrap = cur_is_body
            && next_is_body
            && !cur.is_tabular
            && !next.is_tabular
            && gap > 0.0
            && gap <= max_gap
            // TS .length is UTF-16 code units — not bytes ("—" is 1 unit, 3 bytes)
            && cur.content.encode_utf16().count() > min_wrap_length
            && !sentence_end_re.is_match(&cur.content);

        if is_wrap {
            cur = Cur {
                top_y: cur.top_y,
                last_top_y: next.top_y,
                content: format!("{} {}", cur.content.trim_end(), next.content.trim_start()),
                is_tabular: false,
            };
        } else {
            merged.push(ContentBlock {
                top_y: cur.top_y,
                content: cur.content,
                is_tabular: false,
            });
            cur = Cur {
                top_y: next.top_y,
                last_top_y: next.top_y,
                content: next.content.clone(),
                is_tabular: next.is_tabular,
            };
        }
    }
    merged.push(ContentBlock {
        top_y: cur.top_y,
        content: cur.content,
        is_tabular: false,
    });
    merged
}

/// Remove page number blocks near the bottom of the page.
fn remove_page_numbers(blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
    static PAGE_NUM_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^(?:#{1,6}\s*)?\d+\s*$").unwrap());
    let page_num_re = &*PAGE_NUM_RE;
    let bottom_y = 120.0;
    let len = blocks.len();

    blocks
        .into_iter()
        .enumerate()
        .filter(|(idx, block)| {
            let is_bottom = *idx >= len.saturating_sub(3);
            let is_low_y = block.top_y <= bottom_y;
            let is_page_num = page_num_re.is_match(block.content.trim());
            !(is_bottom && is_low_y && is_page_num)
        })
        .map(|(_, block)| block)
        .collect()
}

// ---------------------------------------------------------------------------
// Detached first-column table reconstruction
// ---------------------------------------------------------------------------

/// Fix tables where the first column was emitted as free text blocks
/// around a markdown table containing only the right-side columns.
///
/// Detects: a plain-text header line with (N+1) tokens above an N-column
/// markdown table, plus short label lines whose count matches the table's
/// logical row count. Reconstructs into a proper (N+1)-column table.
fn normalize_detached_first_column_tables(blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
    static HEADING_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^#{1,6}\s").unwrap());
    let heading_re = &*HEADING_RE;
    static SEPARATOR_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^\|\s*[-: ]+\|").unwrap());
    let separator_re = &*SEPARATOR_RE;

    let is_table_block = |text: &str| text.trim_start().starts_with('|');
    let is_plain_block = |text: &str| !heading_re.is_match(text) && !is_table_block(text);
    let is_short_label = |text: &str| {
        let t = text.trim();
        !t.is_empty() && t.len() <= 40
    };
    let split_tokens = |text: &str| -> Vec<String> {
        text.split_whitespace()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    };

    let mut replacements: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    let mut remove: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for table_idx in 0..blocks.len() {
        if remove.contains(&table_idx) {
            continue;
        }
        let table_block = &blocks[table_idx];
        if !is_table_block(&table_block.content) {
            continue;
        }

        let table_lines: Vec<String> = table_block
            .content
            .split('\n')
            .map(|line| line.trim().to_string())
            .filter(|line| line.starts_with('|'))
            .collect();

        let data_rows: Vec<Vec<String>> = table_lines
            .iter()
            .filter(|line| !separator_re.is_match(line))
            .map(|line| parse_pipe_row(line))
            .filter(|row| !row.is_empty())
            .collect();

        if data_rows.is_empty() {
            continue;
        }
        let cols = data_rows[0].len();
        if cols < 2 || data_rows.iter().any(|row| row.len() != cols) {
            continue;
        }

        // Expand by <br> count to get logical row count
        let mut logical_rows: Vec<Vec<String>> = Vec::new();
        for row in &data_rows {
            let split_cells: Vec<Vec<String>> = row
                .iter()
                .map(|cell| cell.split("<br>").map(|p| p.trim().to_string()).collect())
                .collect();
            let row_span = split_cells
                .iter()
                .map(|parts| parts.len())
                .max()
                .unwrap_or(1);
            for k in 0..row_span {
                logical_rows.push(
                    split_cells
                        .iter()
                        .map(|parts| parts.get(k).cloned().unwrap_or_default())
                        .collect(),
                );
            }
        }
        if logical_rows.len() < 2 {
            continue;
        }

        // Find header with (cols + 1) non-numeric tokens
        let mut header_idx: Option<usize> = None;
        let mut header_tokens: Vec<String> = Vec::new();
        let start = table_idx.saturating_sub(4);
        for i in start..table_idx {
            let text = normalize_full_width_ascii(blocks[i].content.trim());
            if !is_plain_block(&text) {
                continue;
            }
            let tokens = split_tokens(&text);
            if tokens.len() == cols + 1
                && tokens
                    .iter()
                    .all(|tok| !tok.chars().any(|c| c.is_ascii_digit()))
            {
                header_idx = Some(i);
                header_tokens = tokens;
            }
        }
        let header_idx = match header_idx {
            Some(idx) => idx,
            None => continue,
        };

        // Collect short label lines above/below table
        let mut above_labels: Vec<(usize, String)> = Vec::new();
        for i in (header_idx + 1..table_idx).rev() {
            let text = normalize_full_width_ascii(blocks[i].content.trim());
            if !is_plain_block(&text) || !is_short_label(&text) {
                break;
            }
            above_labels.push((i, text));
        }
        above_labels.reverse();

        let mut below_labels: Vec<(usize, String)> = Vec::new();
        for i in (table_idx + 1)..blocks.len() {
            let text = normalize_full_width_ascii(blocks[i].content.trim());
            if !is_plain_block(&text) || !is_short_label(&text) {
                break;
            }
            below_labels.push((i, text));
        }

        let mut labels: Vec<(usize, String)> = Vec::new();
        labels.extend(above_labels);
        labels.extend(below_labels);

        if labels.len() != logical_rows.len() {
            continue;
        }

        // Reconstruct the full table
        let mut normalized_lines: Vec<String> = Vec::new();
        normalized_lines.push(format!("| {} |", header_tokens.join(" | ")));
        normalized_lines.push(format!("| {} |", vec!["---"; cols + 1].join(" | ")));
        for (r, logical_row) in logical_rows.iter().enumerate() {
            normalized_lines.push(format!("| {} | {} |", labels[r].1, logical_row.join(" | ")));
        }

        replacements.insert(table_idx, normalized_lines.join("\n"));
        remove.insert(header_idx);
        for (idx, _) in &labels {
            remove.insert(*idx);
        }
    }

    if replacements.is_empty() && remove.is_empty() {
        return blocks;
    }

    let mut out: Vec<ContentBlock> = Vec::new();
    for (i, block) in blocks.into_iter().enumerate() {
        if remove.contains(&i) {
            continue;
        }
        if let Some(replaced) = replacements.get(&i) {
            out.push(ContentBlock {
                top_y: block.top_y,
                content: replaced.clone(),
                is_tabular: false,
            });
        } else {
            out.push(block);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// An image block with position and pre-rendered markdown.
pub struct ImageBlock {
    pub top_y: f64,
    pub markdown: String,
}

/// Render one page's content: free text and tables interleaved top-to-bottom.
pub fn render_page_content(
    free_text_boxes: &[TextBox],
    tables: &[TableGrid],
    image_blocks: &[ImageBlock],
    all_text_boxes: Option<&[TextBox]>,
) -> String {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    // Use ALL text boxes (before table/diagram filtering) for modal font size,
    // so that diagram labels released as free text don't skew the body size.
    let body_fs = modal_font_size(all_text_boxes.unwrap_or(free_text_boxes));

    // Free text lines
    for line in group_free_text_into_lines(free_text_boxes) {
        let prefix = heading_prefix(line.font_size, body_fs, line.is_bold);
        blocks.push(ContentBlock {
            top_y: line.top_y,
            content: format!("{}{}", prefix, line.text),
            is_tabular: prefix.is_empty() && line.is_tabular,
        });
    }

    // Tables
    for table in tables {
        let md = render_table_to_markdown(table);
        if !md.is_empty() {
            blocks.push(ContentBlock {
                top_y: table.top_y,
                content: md,
                is_tabular: false,
            });
        }
    }

    // Images
    for img in image_blocks {
        blocks.push(ContentBlock {
            top_y: img.top_y,
            content: img.markdown.clone(),
            is_tabular: false,
        });
    }

    // Sort top-to-bottom (higher Y = higher on page = comes first)
    blocks.sort_by(|a, b| {
        b.top_y
            .partial_cmp(&a.top_y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let cleaned = remove_page_numbers(blocks);
    let headings_merged = merge_consecutive_headings(cleaned, body_fs);
    let merged = merge_paragraph_wraps(headings_merged, body_fs);
    let normalized = normalize_detached_first_column_tables(merged);

    normalized
        .iter()
        .map(|b| b.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    use std::sync::atomic::{AtomicU32, Ordering};
    static ID_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn reset_id() {
        ID_COUNTER.store(0, Ordering::SeqCst);
    }

    fn make_box(
        text: &str,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        font_size: f64,
        is_bold: bool,
    ) -> TextBox {
        let id = ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        TextBox {
            id: format!("t{}", id),
            text: text.to_string(),
            page_number: 1,
            font_size,
            is_bold,
            bounds: Bounds {
                left: x,
                right: x + w,
                bottom: y,
                top: y + h,
            },
        }
    }

    fn bx(text: &str) -> TextBox {
        make_box(text, 100.0, 500.0, 100.0, 10.0, 9.0, false)
    }

    fn bx_at(text: &str, x: f64, y: f64) -> TextBox {
        make_box(text, x, y, 100.0, 10.0, 9.0, false)
    }

    fn bx_y(text: &str, y: f64) -> TextBox {
        make_box(text, 100.0, y, 100.0, 10.0, 9.0, false)
    }

    fn bx_font(text: &str, y: f64, font_size: f64) -> TextBox {
        make_box(text, 100.0, y, 100.0, 10.0, font_size, false)
    }

    fn bx_bold_font(text: &str, y: f64, font_size: f64) -> TextBox {
        make_box(text, 100.0, y, 100.0, 10.0, font_size, true)
    }

    fn make_grid(overrides: Option<TableGridOverrides>) -> TableGrid {
        let o = overrides.unwrap_or_default();
        TableGrid {
            page_number: 1,
            rows: o.rows.unwrap_or(2),
            cols: o.cols.unwrap_or(2),
            top_y: o.top_y.unwrap_or(300.0),
            warnings: vec![],
            is_borderless: false,
            cells: o.cells.unwrap_or_else(|| {
                vec![
                    TableCell {
                        row: 0,
                        col: 0,
                        text: "Name".to_string(),
                        row_span: 1,
                        col_span: 1,
                    },
                    TableCell {
                        row: 0,
                        col: 1,
                        text: "Role".to_string(),
                        row_span: 1,
                        col_span: 1,
                    },
                    TableCell {
                        row: 1,
                        col: 0,
                        text: "Alice".to_string(),
                        row_span: 1,
                        col_span: 1,
                    },
                    TableCell {
                        row: 1,
                        col: 1,
                        text: "CEO".to_string(),
                        row_span: 1,
                        col_span: 1,
                    },
                ]
            }),
        }
    }

    #[derive(Default)]
    struct TableGridOverrides {
        rows: Option<usize>,
        cols: Option<usize>,
        top_y: Option<f64>,
        cells: Option<Vec<TableCell>>,
    }

    // -----------------------------------------------------------------------
    // renderTableToMarkdown
    // -----------------------------------------------------------------------

    #[test]
    fn renders_a_2x2_table() {
        reset_id();
        let md = render_table_to_markdown(&make_grid(None));
        assert!(md.contains("| Name | Role |"));
        assert!(md.contains("| --- | --- |"));
        assert!(md.contains("| Alice | CEO |"));
    }

    #[test]
    fn returns_empty_string_for_rows_0() {
        reset_id();
        let g = make_grid(Some(TableGridOverrides {
            rows: Some(0),
            cells: Some(vec![]),
            ..Default::default()
        }));
        assert_eq!(render_table_to_markdown(&g), "");
    }

    #[test]
    fn returns_empty_string_for_cols_0() {
        reset_id();
        let g = make_grid(Some(TableGridOverrides {
            cols: Some(0),
            cells: Some(vec![]),
            ..Default::default()
        }));
        assert_eq!(render_table_to_markdown(&g), "");
    }

    #[test]
    fn escapes_pipe_characters_in_cell_text() {
        reset_id();
        let g = make_grid(Some(TableGridOverrides {
            rows: Some(1),
            cols: Some(1),
            cells: Some(vec![TableCell {
                row: 0,
                col: 0,
                text: "A|B".to_string(),
                row_span: 1,
                col_span: 1,
            }]),
            ..Default::default()
        }));
        assert!(render_table_to_markdown(&g).contains("A\\|B"));
    }

    #[test]
    fn converts_newlines_to_br() {
        reset_id();
        let g = make_grid(Some(TableGridOverrides {
            rows: Some(1),
            cols: Some(1),
            cells: Some(vec![TableCell {
                row: 0,
                col: 0,
                text: "line1\nline2".to_string(),
                row_span: 1,
                col_span: 1,
            }]),
            ..Default::default()
        }));
        assert!(render_table_to_markdown(&g).contains("line1<br>line2"));
    }

    #[test]
    fn normalizes_full_width_ascii_characters() {
        reset_id();
        let g = make_grid(Some(TableGridOverrides {
            rows: Some(1),
            cols: Some(1),
            cells: Some(vec![TableCell {
                row: 0,
                col: 0,
                text: "Ａ＋Ｂ".to_string(),
                row_span: 1,
                col_span: 1,
            }]),
            ..Default::default()
        }));
        assert!(render_table_to_markdown(&g).contains("A+B"));
    }

    #[test]
    fn renders_a_single_row_table_header_only() {
        reset_id();
        let g = make_grid(Some(TableGridOverrides {
            rows: Some(1),
            cols: Some(2),
            cells: Some(vec![
                TableCell {
                    row: 0,
                    col: 0,
                    text: "Col A".to_string(),
                    row_span: 1,
                    col_span: 1,
                },
                TableCell {
                    row: 0,
                    col: 1,
                    text: "Col B".to_string(),
                    row_span: 1,
                    col_span: 1,
                },
            ]),
            ..Default::default()
        }));
        let md = render_table_to_markdown(&g);
        assert!(md.contains("| Col A | Col B |"));
        assert!(md.contains("| --- | --- |"));
    }

    // -----------------------------------------------------------------------
    // renderPageContent: free text
    // -----------------------------------------------------------------------

    #[test]
    fn outputs_plain_text() {
        reset_id();
        let result = render_page_content(&[bx("Hello world")], &[], &[], None);
        assert!(result.contains("Hello world"));
    }

    #[test]
    fn merges_text_boxes_on_the_same_y_line() {
        reset_id();
        let boxes = vec![
            bx_at("first", 100.0, 500.0),
            bx_at("second", 220.0, 501.0), // Y diff=1 → same line
        ];
        let result = render_page_content(&boxes, &[], &[], None);
        assert!(result.contains("first second"));
    }

    #[test]
    fn separates_text_boxes_on_different_y_lines() {
        reset_id();
        let boxes = vec![bx_y("line one", 600.0), bx_y("line two", 500.0)];
        let result = render_page_content(&boxes, &[], &[], None);
        assert!(result.contains("line one"));
        assert!(result.contains("line two"));
        // line one should come before line two (higher Y = earlier)
        assert!(result.find("line one").unwrap() < result.find("line two").unwrap());
    }

    // -----------------------------------------------------------------------
    // renderPageContent: heading detection
    // -----------------------------------------------------------------------

    #[test]
    fn large_font_becomes_h1_heading() {
        reset_id();
        let boxes = vec![
            bx_font("Body text", 400.0, 9.0),
            bx_font("Big Title", 600.0, 20.0),
        ];
        let result = render_page_content(&boxes, &[], &[], None);
        assert!(result.contains("# Big Title"));
    }

    #[test]
    fn medium_font_becomes_h2_heading() {
        reset_id();
        let boxes = vec![
            bx_font("Body text", 400.0, 9.0),
            bx_font("Section", 600.0, 14.0),
        ];
        let result = render_page_content(&boxes, &[], &[], None);
        assert!(result.contains("## Section"));
    }

    #[test]
    fn bold_slightly_larger_font_becomes_h3_heading() {
        reset_id();
        let boxes = vec![
            bx_font("Body text", 400.0, 9.0),
            bx_bold_font("Subsection", 600.0, 11.0),
        ];
        let result = render_page_content(&boxes, &[], &[], None);
        assert!(result.contains("### Subsection"));
    }

    #[test]
    fn different_heading_levels_do_not_merge() {
        reset_id();
        let boxes = vec![
            bx_font("Body 1", 200.0, 9.0),
            bx_font("Body 2", 180.0, 9.0),
            bx_font("Body 3", 160.0, 9.0),
            bx_font("Chapter Title", 700.0, 20.0), // # (ratio 2.2)
            bx_font("Section Title", 670.0, 14.0), // ## (ratio 1.5)
        ];
        let result = render_page_content(&boxes, &[], &[], None);
        assert!(result.contains("# Chapter Title"));
        assert!(result.contains("## Section Title"));
        // They should be separate headings, not merged
        // Exact line-start matching (## is a substring of #)
        let h1_count_real = result.lines().filter(|l| l.starts_with("# ")).count();
        let h2_count_real = result.lines().filter(|l| l.starts_with("## ")).count();
        assert_eq!(h1_count_real, 1);
        assert_eq!(h2_count_real, 1);
    }

    #[test]
    fn same_size_text_does_not_become_a_heading() {
        reset_id();
        let boxes = vec![
            bx_font("Regular A", 600.0, 9.0),
            bx_font("Regular B", 500.0, 9.0),
        ];
        let result = render_page_content(&boxes, &[], &[], None);
        assert!(!result.lines().any(|l| l.starts_with('#')));
    }

    #[test]
    fn merges_consecutive_same_level_headings_wrapped_title() {
        reset_id();
        // Need enough body-sized boxes to establish modal font size
        let boxes = vec![
            bx_font("Body line 1", 300.0, 9.0),
            bx_font("Body line 2", 280.0, 9.0),
            bx_font("Body line 3", 260.0, 9.0),
            bx_font("Long Title Part One", 620.0, 20.0),
            bx_font("Part Two of Title", 605.0, 20.0),
        ];
        let result = render_page_content(&boxes, &[], &[], None);
        // Both parts should be in a single # heading, joined with a space
        assert!(result.contains("# Long Title Part One Part Two of Title"));
        let heading_count = result.lines().filter(|l| l.starts_with("# ")).count();
        assert_eq!(heading_count, 1);
    }

    // -----------------------------------------------------------------------
    // renderPageContent: text + tables ordering
    // -----------------------------------------------------------------------

    #[test]
    fn orders_text_and_tables_by_y_position() {
        reset_id();
        let title = bx_font("Title", 700.0, 20.0);
        let g = make_grid(Some(TableGridOverrides {
            top_y: Some(300.0),
            ..Default::default()
        }));
        let result = render_page_content(&[title], &[g], &[], None);
        let title_pos = result.find("Title").unwrap();
        let table_pos = result.find("| Name |").unwrap();
        assert!(title_pos < table_pos);
    }

    #[test]
    fn includes_both_text_and_table_content() {
        reset_id();
        let result =
            render_page_content(&[bx_y("Some text", 600.0)], &[make_grid(None)], &[], None);
        assert!(result.contains("Some text"));
        assert!(result.contains("| Name | Role |"));
    }

    // -----------------------------------------------------------------------
    // renderPageContent: image blocks
    // -----------------------------------------------------------------------

    #[test]
    fn includes_image_markdown_at_correct_position() {
        reset_id();
        let title = bx_font("Section Title", 700.0, 20.0);
        let body = bx_y("Body text below", 300.0);
        let image_blocks = vec![ImageBlock {
            top_y: 500.0,
            markdown: "![diagram](images/fig1.png)".to_string(),
        }];
        let result = render_page_content(&[title, body], &[], &image_blocks, None);

        assert!(result.contains("![diagram](images/fig1.png)"));
        // Image should be between title and body
        let title_pos = result.find("Section Title").unwrap();
        let img_pos = result.find("![diagram]").unwrap();
        let body_pos = result.find("Body text below").unwrap();
        assert!(title_pos < img_pos);
        assert!(img_pos < body_pos);
    }

    #[test]
    fn includes_html_comment_placeholders() {
        reset_id();
        let image_blocks = vec![ImageBlock {
            top_y: 500.0,
            markdown: "<!-- image: p5-img0 (page 5, 400x200pt) -->".to_string(),
        }];
        let result = render_page_content(&[], &[], &image_blocks, None);
        assert!(result.contains("<!-- image: p5-img0"));
    }

    // -----------------------------------------------------------------------
    // renderPageContent: page number removal
    // -----------------------------------------------------------------------

    #[test]
    fn removes_standalone_page_numbers_at_the_bottom() {
        reset_id();
        let boxes = vec![
            bx_y("Real content", 500.0),
            bx_y("42", 50.0), // bottom of page, looks like page number
        ];
        let result = render_page_content(&boxes, &[], &[], None);
        assert!(result.contains("Real content"));
        let has_42 = regex::Regex::new(r"\b42\b").unwrap();
        assert!(!has_42.is_match(&result));
    }

    #[test]
    fn does_not_remove_numbers_that_are_part_of_content() {
        reset_id();
        let boxes = vec![bx_y("There are 42 items", 500.0)];
        let result = render_page_content(&boxes, &[], &[], None);
        assert!(result.contains("42"));
    }
}
