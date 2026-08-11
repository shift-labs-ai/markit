/**
 * Multi-column layout detection and text box reordering.
 *
 * Reading order is recursive whitespace decomposition (an XY-cut with
 * crossing tolerance):
 *
 *   1. Split the region at every horizontal whitespace band tall
 *      enough to be structural (≥ 2.5 line heights) — stacked slices
 *      in top-to-bottom order. A vertical segment spanning the band
 *      (a table border) vetoes the split.
 *   2. Within a slice, find vertical gutters via an x-coverage
 *      histogram: a gutter is a run of bins that almost no box
 *      crosses (full-width titles and headings may cross), wide
 *      enough, with enough boxes fully on each side. More qualifying
 *      gutters than any text layout has columns means table columns —
 *      the region stays whole. A ruled grid covering most of the
 *      region's text also keeps it whole.
 *   3. Boxes crossing a gutter are full-width "bands"; they partition
 *      the slice vertically. Each partition's columns are emitted
 *      left to right — and each column recurses, so a column may
 *      contain its own headings, sub-columns, and structure
 *      (magazine layouts).
 *
 * This only detects the structure. The caller is responsible for
 * processing each group's text boxes independently (table detection,
 * rendering, etc.).
 */

import type { Segment, TextBox } from "./types.js";

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
 * Fraction of the region's boxes allowed to cross a gutter (full-width
 * titles, headings, footnote rules). More crossings than this means the
 * whitespace is coincidental, not structural.
 */
const MAX_CROSSING_FRACTION = 0.15;

/** Maximum number of gutters (three-column layouts). */
const MAX_GUTTERS = 2;

/** Recursion cap for the layout tree. */
const MAX_LAYOUT_DEPTH = 6;

/** A horizontal whitespace band must be at least this tall in points… */
const H_SPLIT_MIN_GAP_PTS = 18;

/**
 * …and at least this many median line-heights, to count as a
 * structural break rather than paragraph leading.
 */
const H_SPLIT_GAP_LINES = 2.5;

export interface ColumnLayout {
  /** Number of groups in reading order (1 = single column). */
  columnCount: number;
  /** Text boxes grouped in reading order. */
  columns: TextBox[][];
  /** True for groups that are full-width bands or horizontal slices. */
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

  // Many parallel qualifying gutters mean the whitespace comes from
  // TABLE columns — leave the region whole so table detection can
  // have it.
  if (centers.length > MAX_GUTTERS) return [];

  centers.sort((a, b) => b.width - a.width);
  return centers
    .slice(0, MAX_GUTTERS)
    .map((c) => c.center)
    .sort((a, b) => a - b);
}

/**
 * Median glyph-box height — the line-height reference for horizontal
 * splitting.
 */
function medianHeight(boxes: TextBox[]): number {
  const heights = boxes
    .map((tb) => tb.bounds.top - tb.bounds.bottom)
    .filter((h) => h > 0)
    .sort((a, b) => a - b);
  if (heights.length === 0) return 10;
  return heights[Math.floor(heights.length / 2)];
}

/**
 * Does a vertical segment span the whitespace band [lower, upper]?
 * A table border crossing the gap proves the region is one object and
 * must not be sliced.
 */
function verticalSegmentSpansGap(
  segments: Segment[],
  lower: number,
  upper: number,
): boolean {
  return segments.some((seg) => {
    if (Math.abs(seg.x1 - seg.x2) > 1) return false;
    const segMin = Math.min(seg.y1, seg.y2);
    const segMax = Math.max(seg.y1, seg.y2);
    return segMin < lower + 1 && segMax > upper - 1;
  });
}

/**
 * Split a region at horizontal whitespace bands taller than the
 * structural threshold. Returns null when the region is one piece.
 */
function horizontalSplits(
  boxes: TextBox[],
  segments: Segment[],
): TextBox[][] | null {
  const threshold = Math.max(
    H_SPLIT_GAP_LINES * medianHeight(boxes),
    H_SPLIT_MIN_GAP_PTS,
  );
  const sorted = [...boxes].sort((a, b) => b.bounds.top - a.bounds.top);

  const parts: TextBox[][] = [];
  let current: TextBox[] = [];
  let minBottom = Number.POSITIVE_INFINITY;
  for (const tb of sorted) {
    if (
      current.length > 0 &&
      minBottom - tb.bounds.top >= threshold &&
      !verticalSegmentSpansGap(segments, tb.bounds.top, minBottom)
    ) {
      parts.push(current);
      current = [];
      minBottom = Number.POSITIVE_INFINITY;
    }
    minBottom = Math.min(minBottom, tb.bounds.bottom);
    current.push(tb);
  }
  if (current.length > 0) parts.push(current);
  return parts.length >= 2 ? parts : null;
}

/**
 * Does a ruled grid (≥2 vertical and ≥2 horizontal segments) cover
 * most of the region's text? Such a region is a table and must stay
 * whole — but a figure's box elsewhere on the page must not freeze
 * column detection, so coverage of the text matters, not presence.
 */
function regionHasRuledGrid(boxes: TextBox[], segments: Segment[]): boolean {
  if (segments.length < 4 || boxes.length === 0) return false;
  const xMin = Math.min(...boxes.map((tb) => tb.bounds.left));
  const xMax = Math.max(...boxes.map((tb) => tb.bounds.right));
  const yMin = Math.min(...boxes.map((tb) => tb.bounds.bottom));
  const yMax = Math.max(...boxes.map((tb) => tb.bounds.top));
  const overlaps = (seg: Segment): boolean =>
    Math.max(seg.x1, seg.x2) >= xMin &&
    Math.min(seg.x1, seg.x2) <= xMax &&
    Math.max(seg.y1, seg.y2) >= yMin &&
    Math.min(seg.y1, seg.y2) <= yMax;
  const vertical = segments.filter(
    (s) => Math.abs(s.x1 - s.x2) <= 1 && overlaps(s),
  );
  const horizontal = segments.filter(
    (s) => Math.abs(s.y1 - s.y2) <= 1 && overlaps(s),
  );
  if (vertical.length < 2 || horizontal.length < 2) return false;
  // Grid bbox: union of the qualifying segments.
  const all = [...vertical, ...horizontal];
  const gx0 = Math.min(...all.map((s) => Math.min(s.x1, s.x2)));
  const gx1 = Math.max(...all.map((s) => Math.max(s.x1, s.x2)));
  const gy0 = Math.min(...all.map((s) => Math.min(s.y1, s.y2)));
  const gy1 = Math.max(...all.map((s) => Math.max(s.y1, s.y2)));
  const inside = boxes.filter((tb) => {
    const cx = (tb.bounds.left + tb.bounds.right) / 2;
    const cy = (tb.bounds.top + tb.bounds.bottom) / 2;
    return cx >= gx0 && cx <= gx1 && cy >= gy0 && cy <= gy1;
  }).length;
  return inside * 10 >= boxes.length * 6;
}

/**
 * Recursive layout: horizontal slices first, then tolerant gutter
 * detection, recursing into each column. `isBand` records the leaf's
 * provenance — horizontal slices and crossing boxes are full-width
 * bands, column leaves are not.
 */
function layoutRegion(
  boxes: TextBox[],
  segments: Segment[],
  depth: number,
  isBand: boolean,
  out: Array<{ boxes: TextBox[]; band: boolean }>,
  guttersOut: number[],
): void {
  if (depth >= MAX_LAYOUT_DEPTH || boxes.length < MIN_BOXES_PER_COLUMN * 2) {
    out.push({ boxes, band: isBand });
    return;
  }

  const parts = horizontalSplits(boxes, segments);
  if (parts) {
    for (const part of parts) {
      layoutRegion(part, segments, depth + 1, true, out, guttersOut);
    }
    return;
  }

  // A ruled table region must not be column-split — its interior
  // whitespace belongs to the grid, not the page layout.
  if (regionHasRuledGrid(boxes, segments)) {
    out.push({ boxes, band: isBand });
    return;
  }

  const gutters = findGutters(boxes);
  if (gutters.length === 0) {
    out.push({ boxes, band: isBand });
    return;
  }
  guttersOut.push(...gutters);

  const crossesGutter = (tb: TextBox): boolean =>
    gutters.some((g) => tb.bounds.left + 2 < g && g < tb.bounds.right - 2);
  const columnOf = (tb: TextBox): number => {
    const centerX = (tb.bounds.left + tb.bounds.right) / 2;
    const idx = gutters.findIndex((g) => centerX < g);
    return idx < 0 ? gutters.length : idx;
  };

  // Walk top-to-bottom (Y-up: larger top first). Crossing boxes
  // (bands) flush the open partition; consecutive band boxes group.
  const ordered = [...boxes].sort((a, b) => b.bounds.top - a.bounds.top);

  let regionColumns: TextBox[][] = Array.from(
    { length: gutters.length + 1 },
    () => [],
  );
  let openBand: TextBox[] = [];

  const flushColumns = () => {
    for (const column of regionColumns) {
      if (column.length > 0) {
        layoutRegion(column, segments, depth + 1, false, out, guttersOut);
      }
    }
    regionColumns = Array.from({ length: gutters.length + 1 }, () => []);
  };

  for (const tb of ordered) {
    if (crossesGutter(tb)) {
      flushColumns();
      openBand.push(tb);
    } else {
      if (openBand.length > 0) {
        out.push({ boxes: openBand, band: true });
        openBand = [];
      }
      regionColumns[columnOf(tb)].push(tb);
    }
  }
  flushColumns();
  if (openBand.length > 0) out.push({ boxes: openBand, band: true });
}

/**
 * Detect column layout and return text boxes grouped in reading order.
 *
 * For single-column pages, returns all boxes in one group. For
 * structured pages, returns full-width bands, horizontal slices, and
 * per-region columns (recursively decomposed) in reading order.
 */
export function detectColumns(
  textBoxes: TextBox[],
  segments: Segment[] = [],
): ColumnLayout {
  if (textBoxes.length < MIN_BOXES_PER_COLUMN * 2) return single(textBoxes);

  const groups: Array<{ boxes: TextBox[]; band: boolean }> = [];
  const gutters: number[] = [];
  layoutRegion(textBoxes, segments, 0, false, groups, gutters);

  if (groups.length <= 1) return single(textBoxes);
  gutters.sort((a, b) => a - b);
  const deduped = gutters.filter(
    (g, i) => i === 0 || Math.abs(g - gutters[i - 1]) >= 1,
  );

  return {
    columnCount: groups.length,
    columns: groups.map((g) => g.boxes),
    bands: groups.map((g) => g.band),
    boundaries: deduped,
  };
}
