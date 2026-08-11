import { describe, expect, it } from "bun:test";
import { detectBorderlessTables } from "./borderless.js";
import type { TextBox } from "./types.js";

function tb(id: string, text: string, x: number, y: number, w = 60): TextBox {
  return {
    id,
    text,
    pageNumber: 1,
    fontSize: 9,
    isBold: false,
    bounds: { left: x, right: x + w, bottom: y, top: y + 10 },
  };
}

function alignedTable(): TextBox[] {
  const boxes: TextBox[] = [];
  const headers = ["Name", "Role", "Age"];
  const rows = [
    ["Alice", "CEO", "44"],
    ["Bob", "CTO", "41"],
    ["Carol", "CFO", "39"],
  ];
  headers.forEach((h, c) => {
    boxes.push(tb(`h${c}`, h, 72 + c * 120, 700));
  });
  rows.forEach((row, r) => {
    row.forEach((cell, c) => {
      boxes.push(tb(`r${r}c${c}`, cell, 72 + c * 120, 685 - r * 15));
    });
  });
  return boxes;
}

describe("detectBorderlessTables", () => {
  it("detects an aligned borderless table", () => {
    const boxes = alignedTable();
    const { grids, consumedIds } = detectBorderlessTables(boxes, 1);
    expect(grids).toHaveLength(1);
    const grid = grids[0];
    expect([grid.rows, grid.cols]).toEqual([4, 3]);
    expect(grid.isBorderless).toBe(true);
    expect(consumedIds).toHaveLength(boxes.length);
    const cell = (r: number, c: number) =>
      grid.cells.find((cell) => cell.row === r && cell.col === c)?.text;
    expect(cell(0, 0)).toBe("Name");
    expect(cell(2, 1)).toBe("CTO");
    expect(cell(3, 2)).toBe("39");
  });

  it("clusters multi-word cells into one column", () => {
    // "TAM 107" split into two boxes 5pt apart must still read as one
    // cell; the 120pt gap to the value column is the divider.
    const boxes: TextBox[] = [];
    for (let r = 0; r < 3; r++) {
      const y = 700 - r * 15;
      boxes.push(tb(`a${r}`, "TAM", 72, y, 30));
      boxes.push(tb(`b${r}`, "107", 107, y, 25));
      boxes.push(tb(`c${r}`, "13.5", 252, y, 30));
    }
    const { grids } = detectBorderlessTables(boxes, 1);
    expect(grids).toHaveLength(1);
    expect(grids[0].cols).toBe(2);
    const cell = (r: number, c: number) =>
      grids[0].cells.find((cell) => cell.row === r && cell.col === c)?.text;
    expect(cell(0, 0)).toBe("TAM 107");
    expect(cell(1, 1)).toBe("13.5");
  });

  it("does not turn prose lines into a table", () => {
    const boxes = Array.from({ length: 5 }, (_, i) =>
      tb(`p${i}`, "A full sentence of body prose text.", 72, 700 - i * 15, 400),
    );
    const { grids, consumedIds } = detectBorderlessTables(boxes, 1);
    expect(grids).toHaveLength(0);
    expect(consumedIds).toHaveLength(0);
  });

  it("requires at least three tabular rows", () => {
    const boxes = [
      tb("a0", "Name", 72, 700),
      tb("a1", "Role", 200, 700),
      tb("b0", "Alice", 72, 685),
      tb("b1", "CEO", 200, 685),
    ];
    const { grids } = detectBorderlessTables(boxes, 1);
    expect(grids).toHaveLength(0);
  });

  it("does not join distant tabular regions", () => {
    const boxes = [
      ...alignedTable(),
      tb("x0", "Key", 72, 300),
      tb("x1", "Value", 200, 300),
    ];
    const { grids } = detectBorderlessTables(boxes, 1);
    expect(grids).toHaveLength(1);
    expect(grids[0].rows).toBe(4);
  });
});
