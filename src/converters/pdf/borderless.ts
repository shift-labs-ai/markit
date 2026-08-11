/**
 * Borderless table detection.
 *
 * Ruled tables are recovered from vector segments (`grid.ts`). Tables
 * drawn without any rules — column headers over aligned value columns —
 * leave nothing for segment detection, so they are reconstructed from
 * text geometry alone:
 *
 *   1. Group text boxes into visual rows by Y midpoint.
 *   2. A row is tabular when it has ≥2 fragments, every neighbor pair
 *      separated by a clear cell gap.
 *   3. Runs of ≥3 vertically adjacent tabular rows form a candidate.
 *   4. Fragment x-intervals across the run cluster into columns; the
 *      candidate is accepted when the rows fill the grid densely and
 *      no fragment straddles two columns.
 *
 * Conservative by design: a false table mangles prose with pipe noise,
 * so failed candidates are left as free text.
 */

import type { TableCell, TableGrid, TextBox } from "./types.js";

/** Y midpoint tolerance for grouping boxes onto one visual row. */
const ROW_Y_TOLERANCE = 3.0;

/**
 * Minimum horizontal gap between neighboring fragments for a row to
 * read as cells rather than words of a sentence.
 */
const MIN_CELL_GAP = 15.0;

/** Minimum number of consecutive tabular rows. */
const MIN_ROWS = 3;

/**
 * Maximum vertical gap between consecutive rows, in multiples of the
 * taller row's height.
 */
const MAX_ROW_GAP_RATIO = 2.5;

/** Column count sanity bounds. */
const MIN_COLS = 2;
const MAX_COLS = 12;

/** Minimum fraction of grid cells that must be filled. */
const MIN_FILL_RATIO = 0.55;

interface Row {
  boxes: TextBox[];
  top: number;
  bottom: number;
}

function groupRows(textBoxes: TextBox[]): Row[] {
  const sorted = [...textBoxes].sort((a, b) => {
    const ya = (a.bounds.top + a.bounds.bottom) / 2;
    const yb = (b.bounds.top + b.bounds.bottom) / 2;
    return yb - ya;
  });
  const rows: Row[] = [];
  for (const tb of sorted) {
    const mid = (tb.bounds.top + tb.bounds.bottom) / 2;
    const last = rows[rows.length - 1];
    if (
      last &&
      Math.abs((last.top + last.bottom) / 2 - mid) <= ROW_Y_TOLERANCE
    ) {
      last.boxes.push(tb);
      last.top = Math.max(last.top, tb.bounds.top);
      last.bottom = Math.min(last.bottom, tb.bounds.bottom);
    } else {
      rows.push({ boxes: [tb], top: tb.bounds.top, bottom: tb.bounds.bottom });
    }
  }
  for (const row of rows) {
    row.boxes.sort((a, b) => a.bounds.left - b.bounds.left);
  }
  return rows;
}

/**
 * A row fragment: one or more adjacent boxes forming a single cell
 * candidate (word boxes separated by less than a cell gap).
 */
interface Fragment {
  boxes: TextBox[];
  left: number;
  right: number;
}

function fragmentText(fragment: Fragment): string {
  return fragment.boxes.map((tb) => tb.text).join(" ");
}

/**
 * Cluster a row's boxes (sorted by left) into cell fragments: adjacent
 * boxes closer than the cell gap belong to the same cell.
 */
function rowFragments(row: Row): Fragment[] {
  const fragments: Fragment[] = [];
  for (const tb of row.boxes) {
    const last = fragments[fragments.length - 1];
    if (last && tb.bounds.left - last.right < MIN_CELL_GAP) {
      last.boxes.push(tb);
      last.right = Math.max(last.right, tb.bounds.right);
    } else {
      fragments.push({
        boxes: [tb],
        left: tb.bounds.left,
        right: tb.bounds.right,
      });
    }
  }
  return fragments;
}

function rowIsTabular(row: Row): boolean {
  return row.boxes.length >= 2 && rowFragments(row).length >= 2;
}

/**
 * Cluster the x-intervals of a run's fragments into columns. Returns
 * the [left, right] extents per column, or null when the column count
 * is implausible.
 */
function clusterColumns(rows: Row[]): Array<[number, number]> | null {
  const intervals: Array<[number, number]> = rows
    .flatMap((row) => rowFragments(row))
    .map((fragment) => [fragment.left, fragment.right]);
  intervals.sort((a, b) => a[0] - b[0]);
  const columns: Array<[number, number]> = [];
  for (const [left, right] of intervals) {
    const last = columns[columns.length - 1];
    if (last && left <= last[1] + MIN_CELL_GAP / 2) {
      last[1] = Math.max(last[1], right);
    } else {
      columns.push([left, right]);
    }
  }
  if (columns.length < MIN_COLS || columns.length > MAX_COLS) return null;
  return columns;
}

function columnOf(
  columns: Array<[number, number]>,
  left: number,
  right: number,
): number | null {
  const center = (left + right) / 2;
  const idx = columns.findIndex(([l, r]) => center >= l && center <= r);
  return idx < 0 ? null : idx;
}

/** Maximum header rows absorbed above a detected table body. */
const MAX_HEADER_ROWS = 2;

/**
 * Maximum vertical distance (in row heights) between a header row and
 * the table body.
 */
const MAX_HEADER_GAP_ROWS = 3.0;

/**
 * Absorb up to MAX_HEADER_ROWS rows immediately above the run as
 * header rows. A header row's fragments must map to >=2 distinct
 * columns - a single wide fragment is a caption, not a header.
 */
function absorbHeaderRows(
  rows: Row[],
  runStart: number,
  columns: Array<[number, number]>,
  consumedSoFar: string[],
): Array<Array<{ column: number; text: string; boxes: TextBox[] }>> {
  const consumed = new Set(consumedSoFar);
  const headers: Array<
    Array<{ column: number; text: string; boxes: TextBox[] }>
  > = [];
  let below = runStart;
  for (let n = 0; n < MAX_HEADER_ROWS; n++) {
    if (below === 0) break;
    const candidate = rows[below - 1];
    if (candidate.boxes.some((tb) => consumed.has(tb.id))) break;
    const belowRow = rows[below];
    const gap = candidate.bottom - belowRow.top;
    const rowHeight = Math.max(candidate.top - candidate.bottom, 1);
    if (gap < 0 || gap > MAX_HEADER_GAP_ROWS * rowHeight) break;
    // Assign fragments to columns by center; require >=2 distinct.
    const cells: Array<{ column: number; text: string; boxes: TextBox[] }> = [];
    const colsHit = new Set<number>();
    for (const fragment of rowFragments(candidate)) {
      const center = (fragment.left + fragment.right) / 2;
      let column = columns.findIndex(([l, r]) => center >= l && center <= r);
      if (column < 0) {
        // Nearest column by center distance.
        let bestD = Number.POSITIVE_INFINITY;
        column = 0;
        for (let ci = 0; ci < columns.length; ci++) {
          const c = (columns[ci][0] + columns[ci][1]) / 2;
          const d = Math.abs(c - center);
          if (d < bestD) {
            bestD = d;
            column = ci;
          }
        }
      }
      colsHit.add(column);
      cells.push({
        column,
        text: fragmentText(fragment),
        boxes: fragment.boxes,
      });
    }
    if (colsHit.size < 2) break;
    headers.push(cells);
    below--;
  }
  headers.reverse();
  return headers;
}

/**
 * Detect borderless tables among free text boxes. Returns the grids
 * plus the ids of every consumed text box.
 */
export function detectBorderlessTables(
  textBoxes: TextBox[],
  pageNumber: number,
): { grids: TableGrid[]; consumedIds: string[] } {
  const rows = groupRows(textBoxes);
  const grids: TableGrid[] = [];
  const consumedIds: string[] = [];

  let i = 0;
  while (i < rows.length) {
    if (!rowIsTabular(rows[i])) {
      i++;
      continue;
    }
    // Extend the run over vertically adjacent tabular rows.
    let j = i + 1;
    while (j < rows.length && rowIsTabular(rows[j])) {
      const prev = rows[j - 1];
      const cur = rows[j];
      const gap = prev.bottom - cur.top;
      const rowHeight = Math.max(
        prev.top - prev.bottom,
        cur.top - cur.bottom,
        1,
      );
      if (gap < 0 || gap > MAX_ROW_GAP_RATIO * rowHeight) break;
      j++;
    }
    if (j - i < MIN_ROWS) {
      i++;
      continue;
    }

    // Grouped header rows (multi-column spans, sub-labels) fuse or
    // straddle the body's columns and would spoil the candidate. Retry
    // with leading rows dropped; the dropped rows come back through
    // header absorption below.
    let start = i;
    let built: {
      columns: Array<[number, number]>;
      cells: TableCell[];
    } | null = null;
    while (j - start >= MIN_ROWS) {
      const candidateRun = rows.slice(start, j);
      const columns = clusterColumns(candidateRun);
      if (!columns) {
        start++;
        continue;
      }
      // If dropping the leading row yields a finer column structure,
      // the leading row is a header - drop it here, absorb it below.
      if (j - (start + 1) >= MIN_ROWS) {
        const finer = clusterColumns(rows.slice(start + 1, j));
        if (finer && finer.length > columns.length) {
          start++;
          continue;
        }
      }
      const cells: TableCell[] = [];
      let filled = 0;
      let ok = true;
      outer: for (
        let rowIndex = 0;
        rowIndex < candidateRun.length;
        rowIndex++
      ) {
        const rowCells: Array<string | null> = columns.map(() => null);
        for (const fragment of rowFragments(candidateRun[rowIndex])) {
          const column = columnOf(columns, fragment.left, fragment.right);
          if (column === null) {
            ok = false;
            break outer;
          }
          const text = fragmentText(fragment);
          if (rowCells[column] !== null) {
            rowCells[column] = `${rowCells[column]} ${text}`;
          } else {
            rowCells[column] = text;
            filled++;
          }
        }
        for (let column = 0; column < rowCells.length; column++) {
          cells.push({
            row: rowIndex,
            col: column,
            text: rowCells[column] ?? "",
            rowSpan: 1,
            colSpan: 1,
          });
        }
      }
      const total = (j - start) * columns.length;
      if (ok && filled >= MIN_FILL_RATIO * total) {
        built = { columns, cells };
        break;
      }
      start++;
    }
    if (!built) {
      i = Math.max(j, i + 1);
      continue;
    }
    const { columns, cells } = built;
    const run = rows.slice(start, j);

    // Header rows above the body join the grid so column-heading
    // relations survive.
    const headers = absorbHeaderRows(rows, start, columns, consumedIds);
    const headerCount = headers.length;
    if (headerCount > 0) {
      for (const cell of cells) cell.row += headerCount;
      for (let rowIndex = 0; rowIndex < headers.length; rowIndex++) {
        const texts: string[] = columns.map(() => "");
        for (const { column, text, boxes } of headers[rowIndex]) {
          texts[column] =
            texts[column] === "" ? text : `${texts[column]} ${text}`;
          for (const tb of boxes) consumedIds.push(tb.id);
        }
        for (let column = 0; column < texts.length; column++) {
          cells.push({
            row: rowIndex,
            col: column,
            text: texts[column],
            rowSpan: 1,
            colSpan: 1,
          });
        }
      }
    }

    const topY = Math.max(
      ...run.flatMap((row) => row.boxes).map((tb) => tb.bounds.top),
    );
    grids.push({
      pageNumber,
      rows: run.length + headerCount,
      cols: columns.length,
      cells,
      warnings: [],
      topY,
      isBorderless: true,
    });
    for (const row of run) {
      for (const tb of row.boxes) consumedIds.push(tb.id);
    }
    i = j;
  }

  return { grids, consumedIds };
}
