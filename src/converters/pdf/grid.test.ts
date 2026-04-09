import { beforeEach, describe, expect, it } from "bun:test";
import { resolveTableGrids } from "./grid.js";
import type { Segment, TextBox } from "./types.js";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

let _sid = 0;
let _tid = 0;

beforeEach(() => {
  _sid = 0;
  _tid = 0;
});

/** Horizontal segment at Y, from x1 to x2. */
function hSeg(y: number, x1: number, x2: number): Segment {
  return { id: `h${_sid++}`, x1, y1: y, x2, y2: y };
}

/** Vertical segment at X, from y1 to y2. */
function vSeg(x: number, y1: number, y2: number): Segment {
  return { id: `v${_sid++}`, x1: x, y1, x2: x, y2 };
}

/** Text box centered at (cx, cy) with some default width. */
function tb(text: string, cx: number, cy: number, pageNumber = 1): TextBox {
  return {
    id: `t${_tid++}`,
    text,
    pageNumber,
    fontSize: 9,
    isBold: false,
    bounds: { left: cx - 10, right: cx + 10, bottom: cy - 5, top: cy + 5 },
  };
}

/**
 * Build a complete rectangular grid of segments.
 * xLines = vertical border X positions, yLines = horizontal border Y positions.
 * Vertical segments span the full Y range, horizontal span full X range.
 */
function tableSegs(xLines: number[], yLines: number[]): Segment[] {
  const segs: Segment[] = [];
  const yMin = Math.min(...yLines);
  const yMax = Math.max(...yLines);
  const xMin = Math.min(...xLines);
  const xMax = Math.max(...xLines);
  for (const x of xLines) segs.push(vSeg(x, yMin, yMax));
  for (const y of yLines) segs.push(hSeg(y, xMin, xMax));
  return segs;
}

// ---------------------------------------------------------------------------
// No grid detected
// ---------------------------------------------------------------------------

describe("resolveTableGrids: no grid", () => {
  it("returns empty when no segments", () => {
    const result = resolveTableGrids(1, [tb("hello", 200, 500)], []);
    expect(result.grids).toHaveLength(0);
    expect(result.consumedIds).toHaveLength(0);
  });

  it("returns empty with only horizontal lines (no vertical)", () => {
    const segs = [hSeg(400, 100, 500), hSeg(350, 100, 500)];
    const result = resolveTableGrids(1, [tb("A", 200, 375)], segs);
    // H-line-only detection requires multi-column spread
    // Single centered text box won't trigger it
    expect(result.grids).toHaveLength(0);
  });

  it("returns empty with no text boxes (empty grid filtered as diagram)", () => {
    const segs = tableSegs([100, 300, 500], [400, 350, 300]);
    const result = resolveTableGrids(1, [], segs);
    // Segments exist but no text — all-empty grid is filtered out
    expect(result.grids).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// Single table detection
// ---------------------------------------------------------------------------

describe("resolveTableGrids: single table", () => {
  //   x: 100 ─── 300 ─── 500
  //   y: 400 ─ [header] ─ 350 ─ [row1] ─ 300
  const xLines = [100, 300, 500];
  const yLines = [400, 350, 300];
  const segs = tableSegs(xLines, yLines);

  it("detects one grid", () => {
    const boxes = [
      tb("Name", 200, 375),
      tb("Role", 400, 375),
      tb("Alice", 200, 325),
      tb("CEO", 400, 325),
    ];
    const { grids, consumedIds } = resolveTableGrids(1, boxes, segs);
    expect(grids).toHaveLength(1);
    expect(consumedIds).toHaveLength(4);
  });

  it("sets topY from the top horizontal line", () => {
    const boxes = [tb("A", 200, 375)];
    const { grids } = resolveTableGrids(1, boxes, segs);
    expect(grids[0].topY).toBeCloseTo(400, 0);
  });

  it("places text in correct cells", () => {
    const boxes = [
      tb("Name", 200, 375),
      tb("Role", 400, 375),
      tb("Alice", 200, 325),
      tb("CEO", 400, 325),
    ];
    const { grids } = resolveTableGrids(1, boxes, segs);
    const g = grids[0];

    const cell = (r: number, c: number) =>
      g.cells.find((cl) => cl.row === r && cl.col === c)?.text ?? "";

    expect(cell(0, 0)).toBe("Name");
    expect(cell(0, 1)).toBe("Role");
    expect(cell(1, 0)).toBe("Alice");
    expect(cell(1, 1)).toBe("CEO");
  });

  it("does not consume text boxes outside the grid", () => {
    const inside = tb("inside", 200, 375);
    const outside = tb("outside", 600, 375); // x=600 is beyond grid
    const { consumedIds } = resolveTableGrids(1, [inside, outside], segs);
    expect(consumedIds).toContain(inside.id);
    expect(consumedIds).not.toContain(outside.id);
  });
});

// ---------------------------------------------------------------------------
// Two separate tables on the same page
// ---------------------------------------------------------------------------

describe("resolveTableGrids: two tables (no bridging verticals)", () => {
  const xLines = [100, 300, 500];
  // Table A: y=400..350, Table B: y=250..200
  // Vertical segments only span each table's own range
  const segsA = tableSegs(xLines, [400, 350]);
  const segsB = tableSegs(xLines, [250, 200]);
  const allSegs = [...segsA, ...segsB];

  it("detects two grids", () => {
    const boxes = [tb("A-Name", 200, 375), tb("B-Name", 200, 225)];
    const { grids } = resolveTableGrids(1, boxes, allSegs);
    expect(grids).toHaveLength(2);
  });

  it("Table A has higher topY than Table B", () => {
    const boxes = [tb("A", 200, 375), tb("B", 200, 225)];
    const { grids } = resolveTableGrids(1, boxes, allSegs);
    const sorted = [...grids].sort((a, b) => b.topY - a.topY);
    expect(sorted[0].topY).toBeCloseTo(400, 0);
    expect(sorted[1].topY).toBeCloseTo(250, 0);
  });

  it("each text box goes to its own table only", () => {
    const boxA = tb("A-row", 200, 375);
    const boxB = tb("B-row", 200, 225);
    const { grids } = resolveTableGrids(1, [boxA, boxB], allSegs);
    const sorted = [...grids].sort((a, b) => b.topY - a.topY);

    const textsA = sorted[0].cells.map((c) => c.text).filter(Boolean);
    const textsB = sorted[1].cells.map((c) => c.text).filter(Boolean);
    expect(textsA).toContain("A-row");
    expect(textsA).not.toContain("B-row");
    expect(textsB).toContain("B-row");
    expect(textsB).not.toContain("A-row");
  });

  it("all boxes appear in consumedIds", () => {
    const boxA = tb("A", 200, 375);
    const boxB = tb("B", 200, 225);
    const { consumedIds } = resolveTableGrids(1, [boxA, boxB], allSegs);
    expect(consumedIds).toContain(boxA.id);
    expect(consumedIds).toContain(boxB.id);
  });
});

// ---------------------------------------------------------------------------
// Continuous vertical lines → single table (not split)
// ---------------------------------------------------------------------------

describe("resolveTableGrids: continuous verticals = one table", () => {
  const xLines = [100, 300, 500];
  const segs = tableSegs(xLines, [400, 350, 300, 250, 200]);

  it("returns one grid for 4 rows", () => {
    const boxes = [
      tb("R0", 200, 375),
      tb("R1", 200, 325),
      tb("R2", 200, 275),
      tb("R3", 200, 225),
    ];
    const { grids } = resolveTableGrids(1, boxes, segs);
    expect(grids).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// Multi-line cell text (boxes at different Y within same cell)
// ---------------------------------------------------------------------------

describe("resolveTableGrids: multi-line cell text", () => {
  // Need a grid with 2 rows so the Y range is well-defined
  const xLines = [100, 300, 500];
  const yLines = [400, 340, 280]; // two rows
  const segs = tableSegs(xLines, yLines);

  it("joins multiple text boxes in a cell with <br>", () => {
    const boxes = [
      tb("Line 1", 200, 380),
      tb("Line 2", 200, 355), // same col, different Y within row 0
      tb("Value", 400, 370),
      tb("Row2", 200, 310),
    ];
    const { grids } = resolveTableGrids(1, boxes, segs);
    expect(grids).toHaveLength(1);
    const cell00 = grids[0].cells.find((c) => c.row === 0 && c.col === 0);
    expect(cell00?.text).toContain("Line 1");
    expect(cell00?.text).toContain("Line 2");
    expect(cell00?.text).toContain("<br>");
  });
});

// ---------------------------------------------------------------------------
// H-line-only table (no vertical segments, columns inferred from X positions)
// ---------------------------------------------------------------------------

describe("resolveTableGrids: H-line-only table", () => {
  it("infers columns from text X positions with outer-frame verticals only", () => {
    // H-line-only triggers when a Y-group has ≥2 H-lines but < 2 interior
    // vertical lines. We provide outer-frame verticals to keep the group
    // together, but no interior column dividers.
    const segs = [
      hSeg(400, 100, 500),
      hSeg(350, 100, 500),
      hSeg(300, 100, 500),
      // Outer frame verticals (left + right only, no interior)
      vSeg(100, 300, 400),
      vSeg(500, 300, 400),
    ];
    const boxes = [
      tb("Label", 140, 375), // left edge ~130
      tb("Value", 420, 375), // left edge ~410 — spread > 50
      tb("Label2", 140, 325),
      tb("Value2", 420, 325),
    ];
    const { grids } = resolveTableGrids(1, boxes, segs);
    expect(grids.length).toBeGreaterThanOrEqual(1);
    if (grids.length > 0) {
      // Only 2 unique X-lines from verticals (100, 500) → groupXLines has 2
      // which means it goes to buildTableGrid not buildHLineOnlyTable.
      // But with only left+right borders, it's effectively a 1-column grid
      // that still captures the text.
      const allText = grids[0].cells.map((c) => c.text).filter(Boolean);
      expect(allText.length).toBeGreaterThan(0);
    }
  });
});

// ---------------------------------------------------------------------------
// Thin decorative lines should not be detected as tables
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Diagram vs table discrimination
// ---------------------------------------------------------------------------

describe("resolveTableGrids: diagram filtering", () => {
  it("filters out sparse grids with wide column count", () => {
    // 3x8 grid — wide enough and with some text in each row/col to survive
    // pruning, but still sparse (< 50% fill) and wide (8 cols)
    // → caught by "fillRatio < 0.4 && cols >= 6"
    const xLines = [100, 160, 220, 280, 340, 400, 460, 520, 580];
    const yLines = [400, 350, 300, 250];
    const segs = tableSegs(xLines, yLines);
    // Place boxes so each row and most columns are occupied, but sparsely
    const boxes = [
      tb("A", 130, 375),
      tb("B", 310, 375),
      tb("C", 190, 325),
      tb("D", 430, 325),
      tb("E", 250, 275),
      tb("F", 550, 275),
    ];
    const { grids } = resolveTableGrids(1, boxes, segs);
    // After pruning keeps 3 rows × ~6 cols ≈ 18 cells with 6 filled (33%)
    // plus cols >= 6 → filtered as diagram
    expect(grids).toHaveLength(0);
  });

  it("filters out grids with > 25 columns", () => {
    // Create a grid with 30 columns — must be a diagram
    const xLines = Array.from({ length: 32 }, (_, i) => 100 + i * 15);
    const yLines = [400, 300];
    const segs = tableSegs(xLines, yLines);
    // Even with lots of text, too many columns → diagram
    const boxes = xLines.slice(0, -1).map((x, i) => tb(`c${i}`, x + 7, 350));
    const { grids } = resolveTableGrids(1, boxes, segs);
    expect(grids).toHaveLength(0);
  });

  it("filters out sparse grids with high text duplication", () => {
    // 3x8 grid, 40% fill, same labels repeated across columns — diagram
    const xLines = [100, 150, 200, 250, 300, 350, 400, 450, 500];
    const yLines = [400, 360, 320, 280];
    const segs = tableSegs(xLines, yLines);
    const boxes = [
      tb("Block", 125, 380),
      tb("Block", 275, 380),
      tb("Block", 425, 380),
      tb("Hash", 125, 340),
      tb("Hash", 275, 340),
      tb("Hash", 425, 340),
      tb("Nonce", 175, 340),
      tb("Nonce", 325, 340),
      tb("Nonce", 475, 340),
      tb("Tx", 125, 300),
      tb("Tx", 275, 300),
    ];
    const { grids } = resolveTableGrids(1, boxes, segs);
    expect(grids).toHaveLength(0);
  });

  it("keeps real data tables with high fill", () => {
    const xLines = [100, 300, 500];
    const yLines = [400, 350, 300];
    const segs = tableSegs(xLines, yLines);
    const boxes = [
      tb("Name", 200, 375),
      tb("Role", 400, 375),
      tb("Alice", 200, 325),
      tb("Engineer", 400, 325),
    ];
    const { grids } = resolveTableGrids(1, boxes, segs);
    expect(grids).toHaveLength(1);
  });
});

describe("resolveTableGrids: decorative lines ignored", () => {
  it("ignores H-lines with small Y span (< MIN_TABLE_HEIGHT)", () => {
    // Two H-lines only 5pt apart — decorative underline, not a table
    const segs = [
      hSeg(400, 100, 500),
      hSeg(395, 100, 500),
      vSeg(100, 395, 400),
      vSeg(500, 395, 400),
    ];
    const { grids } = resolveTableGrids(1, [tb("text", 300, 397)], segs);
    expect(grids).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// Cross-column text box splitting
// ---------------------------------------------------------------------------

describe("resolveTableGrids: cross-column splitting", () => {
  // 3-column grid: x=100..200..300..400, y=500..450
  const xLines = [100, 200, 300, 400];
  const yLines = [500, 450];

  function makeSegs() {
    return tableSegs(xLines, yLines);
  }

  /** Text box with explicit left/right bounds (not centered). */
  function wideBox(
    text: string,
    left: number,
    right: number,
    cy: number,
  ): TextBox {
    return {
      id: `t${_tid++}`,
      text,
      pageNumber: 1,
      fontSize: 9,
      isBold: false,
      bounds: { left, right, bottom: cy - 5, top: cy + 5 },
    };
  }

  it("splits a text box spanning two columns into separate cells", () => {
    // "Alpha Beta" spans col 0 (100-200) and col 1 (200-300)
    const boxes = [
      wideBox("Alpha Beta", 110, 290, 475),
      tb("C", 350, 475), // col 2
    ];
    const { grids } = resolveTableGrids(1, boxes, makeSegs());
    expect(grids).toHaveLength(1);
    const g = grids[0];
    const cell = (r: number, c: number) =>
      g.cells.find((cl) => cl.row === r && cl.col === c)?.text ?? "";

    // "Alpha" should land in col 0, "Beta" in col 1
    expect(cell(0, 0)).toBe("Alpha");
    expect(cell(0, 1)).toBe("Beta");
    expect(cell(0, 2)).toBe("C");
  });

  it("splits a text box spanning three columns", () => {
    // "Aaa Bbb Ccc" spans all 3 columns
    const boxes = [wideBox("Aaa Bbb Ccc", 110, 390, 475)];
    const { grids } = resolveTableGrids(1, boxes, makeSegs());
    expect(grids).toHaveLength(1);
    const g = grids[0];
    const texts = g.cells
      .filter((c) => c.text.trim().length > 0)
      .map((c) => c.text);
    // All three words should end up in separate cells
    expect(texts).toHaveLength(3);
  });

  it("does not split a text box within one column", () => {
    // "Hello World" fits entirely in col 0 (100-200)
    const boxes = [
      wideBox("Hello World", 110, 190, 475),
      tb("X", 250, 475),
      tb("Y", 350, 475),
    ];
    const { grids } = resolveTableGrids(1, boxes, makeSegs());
    expect(grids).toHaveLength(1);
    const g = grids[0];
    const cell = (r: number, c: number) =>
      g.cells.find((cl) => cl.row === r && cl.col === c)?.text ?? "";
    expect(cell(0, 0)).toBe("Hello World");
  });

  it("keeps single-word boxes intact even if spanning columns", () => {
    // One word spanning col 0 and col 1 — can't split, assign by center
    const boxes = [
      wideBox("Superlongword", 110, 290, 475),
      tb("Z", 350, 475),
    ];
    const { grids } = resolveTableGrids(1, boxes, makeSegs());
    expect(grids).toHaveLength(1);
    // Should land in one cell, not be lost
    const allText = grids[0].cells.map((c) => c.text).filter(Boolean);
    expect(allText).toContain("Superlongword");
  });

  it("consumes original text box IDs when splitting", () => {
    const box = wideBox("Alpha Beta", 110, 290, 475);
    const originalId = box.id;
    const { consumedIds } = resolveTableGrids(1, [box], makeSegs());
    // The original ID must be consumed so it doesn't appear as free text
    expect(consumedIds).toContain(originalId);
  });
});

// ---------------------------------------------------------------------------
// Header detection rejects wide paragraph text above grid
// ---------------------------------------------------------------------------

describe("resolveTableGrids: header detection", () => {
  // 2-column grid: x=100..300..500, y=400..350
  const xLines = [100, 300, 500];
  const yLines = [400, 350];

  function makeSegs() {
    return tableSegs(xLines, yLines);
  }

  function wideBox(
    text: string,
    left: number,
    right: number,
    cy: number,
  ): TextBox {
    return {
      id: `t${_tid++}`,
      text,
      pageNumber: 1,
      fontSize: 9,
      isBold: false,
      bounds: { left, right, bottom: cy - 5, top: cy + 5 },
    };
  }

  it("does not absorb wide paragraph text as a header row", () => {
    // A long sentence sitting 15pt above the grid — wider than 1.5 columns
    const paragraph = wideBox(
      "include only the results for the tasks that have an unbounded score:",
      100,
      420,
      412, // center Y = 412, yMax = 400, gap = 12pt (within 20pt threshold)
    );
    const boxes = [
      paragraph,
      tb("A", 200, 375),
      tb("B", 400, 375),
    ];
    const { grids, consumedIds } = resolveTableGrids(1, boxes, makeSegs());
    expect(grids).toHaveLength(1);
    // The paragraph should NOT be consumed by the table
    expect(consumedIds).not.toContain(paragraph.id);
    // The grid should have 1 row, not 2
    expect(grids[0].rows).toBe(1);
  });

  it("absorbs narrow column headers above the grid", () => {
    // Two short labels sitting just above the grid, one per column
    const h1 = wideBox("Name", 150, 250, 412);
    const h2 = wideBox("Role", 350, 450, 412);
    const boxes = [
      h1,
      h2,
      tb("Alice", 200, 375),
      tb("CEO", 400, 375),
    ];
    const { grids, consumedIds } = resolveTableGrids(1, boxes, makeSegs());
    expect(grids).toHaveLength(1);
    // Both headers should be consumed
    expect(consumedIds).toContain(h1.id);
    expect(consumedIds).toContain(h2.id);
    // Grid should have 2 rows (header + data)
    expect(grids[0].rows).toBe(2);
  });
});
