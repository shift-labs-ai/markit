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
    const run = rows.slice(i, j);
    if (run.length < MIN_ROWS) {
      i++;
      continue;
    }

    const columns = clusterColumns(run);
    if (!columns) {
      i = j;
      continue;
    }

    // Assign fragments to columns; abort on any straddler.
    const cells: TableCell[] = [];
    let filled = 0;
    let ok = true;
    outer: for (let rowIndex = 0; rowIndex < run.length; rowIndex++) {
      const rowCells: Array<string | null> = columns.map(() => null);
      for (const fragment of rowFragments(run[rowIndex])) {
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
    const total = run.length * columns.length;
    if (!ok || filled < MIN_FILL_RATIO * total) {
      i = Math.max(j, i + 1);
      continue;
    }

    const topY = Math.max(
      ...run.flatMap((row) => row.boxes).map((tb) => tb.bounds.top),
    );
    grids.push({
      pageNumber,
      rows: run.length,
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
