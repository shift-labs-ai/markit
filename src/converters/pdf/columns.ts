/**
 * Multi-column layout detection and text box reordering.
 *
 * Many PDFs (legal documents, datasheets, academic papers) use two-column
 * layouts. Without column detection, text boxes are ordered by Y position
 * only, interleaving left and right column content.
 *
 * Algorithm:
 *   1. Collect left edges of all text boxes on the page
 *   2. Find the largest horizontal gap between consecutive left edges
 *   3. If gap > MIN_GAP_RATIO of the text width and both sides have
 *      enough boxes → multi-column detected
 *   4. Assign each text box to a column based on its center X
 *   5. Return columns in reading order (left-to-right, top-to-bottom)
 *
 * This only detects the column structure. The caller is responsible for
 * processing each column's text boxes independently (table detection,
 * rendering, etc.).
 */

import type { TextBox } from "./types.js";

/**
 * Minimum gap as a fraction of the total text width to consider a column
 * boundary. A two-column layout typically has ~50% gap; we use a lower
 * threshold to catch asymmetric columns.
 */
const MIN_GAP_RATIO = 0.15;

/** Minimum number of text boxes on each side of the gap. */
const MIN_BOXES_PER_COLUMN = 4;

/** Minimum gap in absolute points to avoid splitting on small whitespace. */
const MIN_GAP_PTS = 40;

export interface ColumnLayout {
  /** Number of columns detected (1 = single column, 2+ = multi-column). */
  columnCount: number;
  /** Text boxes grouped by column, in reading order (left to right). */
  columns: TextBox[][];
  /** X positions of column boundaries (between columns). */
  boundaries: number[];
}

/**
 * Detect column layout and return text boxes grouped by column.
 *
 * For single-column pages, returns all boxes in one group.
 * For multi-column pages, returns boxes split by column in reading order.
 */
export function detectColumns(textBoxes: TextBox[]): ColumnLayout {
  if (textBoxes.length < MIN_BOXES_PER_COLUMN * 2) {
    return { columnCount: 1, columns: [textBoxes], boundaries: [] };
  }

  // Collect unique left edges (rounded to avoid float noise)
  const lefts = [
    ...new Set(textBoxes.map((tb) => Math.round(tb.bounds.left))),
  ].sort((a, b) => a - b);

  if (lefts.length < 2) {
    return { columnCount: 1, columns: [textBoxes], boundaries: [] };
  }

  const textXMin = lefts[0];
  const textXMax = Math.max(
    ...textBoxes.map((tb) => Math.round(tb.bounds.right)),
  );
  const textWidth = textXMax - textXMin;

  if (textWidth <= 0) {
    return { columnCount: 1, columns: [textBoxes], boundaries: [] };
  }

  const boundaries: number[] = [];
  for (let i = 1; i < lefts.length; i++) {
    const gap = lefts[i] - lefts[i - 1];
    if (gap >= MIN_GAP_PTS && gap / textWidth >= MIN_GAP_RATIO) {
      boundaries.push((lefts[i - 1] + lefts[i]) / 2);
    }
  }

  if (boundaries.length === 0) {
    return { columnCount: 1, columns: [textBoxes], boundaries: [] };
  }

  const columns: TextBox[][] = Array.from(
    { length: boundaries.length + 1 },
    () => [],
  );
  for (const box of textBoxes) {
    const centerX = (box.bounds.left + box.bounds.right) / 2;
    const column = boundaries.findIndex((boundary) => centerX < boundary);
    columns[column < 0 ? columns.length - 1 : column].push(box);
  }

  if (columns.some((column) => column.length < MIN_BOXES_PER_COLUMN)) {
    return { columnCount: 1, columns: [textBoxes], boundaries: [] };
  }

  return { columnCount: columns.length, columns, boundaries };
}
