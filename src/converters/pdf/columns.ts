/**
 * Multi-column layout detection and text box reordering.
 *
 * Many PDFs (legal documents, datasheets, academic papers) use two-column
 * layouts. Without column detection, text boxes are ordered by Y position
 * only, interleaving left and right column content.
 *
 * Algorithm (article-level, coverage-based):
 *   1. Build an x-coverage histogram: for each 1pt bin, count the boxes
 *      whose horizontal interval strictly crosses it.
 *   2. A gutter is a run of bins that almost no box crosses (full-width
 *      titles and headings are allowed to cross), wide enough, with
 *      enough boxes fully on each side.
 *   3. Boxes crossing a gutter are full-width "bands" (titles, section
 *      headings, footers). The rest are column-bound.
 *   4. Walk the page top-to-bottom: bands split the page into vertical
 *      regions; each region's boxes are emitted column by column
 *      (left to right), preserving article reading order.
 *
 * This only detects the structure. The caller is responsible for
 * processing each group's text boxes independently (table detection,
 * rendering, etc.).
 */

import type { TextBox } from "./types.js";

/** Minimum number of text boxes fully on each side of a gutter. */
const MIN_BOXES_PER_COLUMN = 4;

/** Minimum gutter width in points. */
const MIN_GUTTER_PTS = 12;

/**
 * Fraction of the text width excluded at each edge when searching for
 * gutters — a gutter in the outer margins is ragged-edge whitespace,
 * not a column separator.
 */
const GUTTER_SEARCH_MARGIN = 0.15;

/**
 * Fraction of the page's boxes allowed to cross a gutter (full-width
 * titles, headings, footnote rules). More crossings than this means the
 * whitespace is coincidental, not structural.
 */
const MAX_CROSSING_FRACTION = 0.15;

/** Maximum number of gutters (three-column layouts). */
const MAX_GUTTERS = 2;

export interface ColumnLayout {
  /** Number of groups in reading order (1 = single column). */
  columnCount: number;
  /** Text boxes grouped in reading order. */
  columns: TextBox[][];
  /** True for groups that are full-width bands (titles, headings). */
  bands: boolean[];
  /** X positions of column gutter centers. */
  boundaries: number[];
}

function single(textBoxes: TextBox[]): ColumnLayout {
  return {
    columnCount: 1,
    columns: [textBoxes],
    bands: [false],
    boundaries: [],
  };
}

/** Gutter centers found via the crossing histogram, best-first capped. */
function findGutters(textBoxes: TextBox[]): number[] {
  const xMin = Math.min(...textBoxes.map((tb) => tb.bounds.left));
  const xMax = Math.max(...textBoxes.map((tb) => tb.bounds.right));
  const width = xMax - xMin;
  if (width <= 0) return [];

  const lo = Math.ceil(xMin + width * GUTTER_SEARCH_MARGIN);
  const hi = Math.floor(xMin + width * (1 - GUTTER_SEARCH_MARGIN));
  if (hi <= lo) return [];

  const maxCrossing = Math.max(
    1,
    Math.floor(textBoxes.length * MAX_CROSSING_FRACTION),
  );

  // Runs of bins crossed by at most maxCrossing boxes.
  const runs: Array<{ start: number; end: number }> = [];
  let runStart: number | null = null;
  for (let x = lo; x <= hi; x++) {
    let crossing = 0;
    for (const tb of textBoxes) {
      if (tb.bounds.left + 2 < x && x < tb.bounds.right - 2) crossing++;
    }
    if (crossing <= maxCrossing) {
      if (runStart === null) runStart = x;
    } else if (runStart !== null) {
      runs.push({ start: runStart, end: x - 1 });
      runStart = null;
    }
  }
  if (runStart !== null) runs.push({ start: runStart, end: hi });

  // Validate: wide enough, and enough boxes fully on each side.
  const centers: Array<{ center: number; width: number }> = [];
  for (const run of runs) {
    const runWidth = run.end - run.start + 1;
    if (runWidth < MIN_GUTTER_PTS) continue;
    const center = (run.start + run.end) / 2;
    const leftCount = textBoxes.filter(
      (tb) => tb.bounds.right <= center,
    ).length;
    const rightCount = textBoxes.filter(
      (tb) => tb.bounds.left >= center,
    ).length;
    if (leftCount < MIN_BOXES_PER_COLUMN || rightCount < MIN_BOXES_PER_COLUMN)
      continue;
    centers.push({ center, width: runWidth });
  }

  centers.sort((a, b) => b.width - a.width);
  return centers
    .slice(0, MAX_GUTTERS)
    .map((c) => c.center)
    .sort((a, b) => a - b);
}

/**
 * Detect column layout and return text boxes grouped in reading order.
 *
 * For single-column pages, returns all boxes in one group. For
 * multi-column pages, returns full-width bands and per-region columns
 * as separate groups in article reading order.
 */
export function detectColumns(textBoxes: TextBox[]): ColumnLayout {
  if (textBoxes.length < MIN_BOXES_PER_COLUMN * 2) return single(textBoxes);

  const gutters = findGutters(textBoxes);
  if (gutters.length === 0) return single(textBoxes);

  const crossesGutter = (tb: TextBox): boolean =>
    gutters.some((g) => tb.bounds.left + 2 < g && g < tb.bounds.right - 2);
  const columnOf = (tb: TextBox): number => {
    const centerX = (tb.bounds.left + tb.bounds.right) / 2;
    const idx = gutters.findIndex((g) => centerX < g);
    return idx < 0 ? gutters.length : idx;
  };

  // Walk top-to-bottom (Y-up: larger top first). Bands flush the open
  // region; consecutive band boxes group together.
  const ordered = [...textBoxes].sort((a, b) => b.bounds.top - a.bounds.top);

  const groups: TextBox[][] = [];
  const bands: boolean[] = [];
  let regionColumns: TextBox[][] = Array.from(
    { length: gutters.length + 1 },
    () => [],
  );
  let openBand: TextBox[] = [];

  const flushRegion = () => {
    for (const column of regionColumns) {
      if (column.length > 0) {
        groups.push(column);
        bands.push(false);
      }
    }
    regionColumns = Array.from({ length: gutters.length + 1 }, () => []);
  };
  const flushBand = () => {
    if (openBand.length > 0) {
      groups.push(openBand);
      bands.push(true);
      openBand = [];
    }
  };

  for (const tb of ordered) {
    if (crossesGutter(tb)) {
      flushRegion();
      openBand.push(tb);
    } else {
      flushBand();
      regionColumns[columnOf(tb)].push(tb);
    }
  }
  flushRegion();
  flushBand();

  if (groups.length <= 1) return single(textBoxes);

  return {
    columnCount: groups.length,
    columns: groups,
    bands,
    boundaries: gutters,
  };
}
