import { describe, expect, it } from "bun:test";
import { detectColumns } from "./columns.js";
import type { TextBox } from "./types.js";

let _id = 0;
function tb(text: string, x: number, y: number, w = 200): TextBox {
  return {
    id: `t${_id++}`,
    text,
    pageNumber: 1,
    fontSize: 9,
    isBold: false,
    bounds: { left: x, right: x + w, bottom: y, top: y + 10 },
  };
}

describe("detectColumns", () => {
  it("returns 1 column for too few boxes", () => {
    const boxes = [tb("A", 100, 500), tb("B", 100, 480)];
    const result = detectColumns(boxes);
    expect(result.columnCount).toBe(1);
    expect(result.columns).toHaveLength(1);
  });

  it("returns 1 column for single-column layout", () => {
    const boxes = Array.from({ length: 20 }, (_, i) =>
      tb(`Line ${i}`, 72, 700 - i * 15),
    );
    const result = detectColumns(boxes);
    expect(result.columnCount).toBe(1);
  });

  it("detects two-column layout", () => {
    // Left column at x=72, right column at x=315 (like the US Constitution)
    const left = Array.from({ length: 10 }, (_, i) =>
      tb(`Left ${i}`, 72, 700 - i * 15),
    );
    const right = Array.from({ length: 10 }, (_, i) =>
      tb(`Right ${i}`, 315, 700 - i * 15),
    );
    const result = detectColumns([...left, ...right]);
    expect(result.columnCount).toBe(2);
    expect(result.columns).toHaveLength(2);
    expect(result.boundaries).toHaveLength(1);
  });

  it("detects three-column layouts", () => {
    const columns = [0, 200, 400].flatMap((x, col) =>
      Array.from({ length: 6 }, (_, i) =>
        tb(`C${col}-${i}`, x, 700 - i * 15, 100),
      ),
    );
    const result = detectColumns(columns);
    expect(result.columnCount).toBe(3);
    expect(result.columns).toHaveLength(3);
    expect(result.boundaries).toHaveLength(2);
  });

  it("left column comes first in reading order", () => {
    const left = Array.from({ length: 10 }, (_, i) =>
      tb(`L${i}`, 72, 700 - i * 15),
    );
    const right = Array.from({ length: 10 }, (_, i) =>
      tb(`R${i}`, 315, 700 - i * 15),
    );
    const result = detectColumns([...right, ...left]); // shuffled input
    expect(result.columns[0].every((b) => b.text.startsWith("L"))).toBe(true);
    expect(result.columns[1].every((b) => b.text.startsWith("R"))).toBe(true);
  });

  it("does not split when gap is too small", () => {
    // Two groups with a small gap — indented text, not real columns
    // Left at x=72 (w=200, right=272), "right" at x=100 (w=200, right=300)
    // Gap between left edges: 100-72=28pt, textWidth=300-72=228, ratio=0.12 < 0.15
    const left = Array.from({ length: 10 }, (_, i) =>
      tb(`A${i}`, 72, 700 - i * 15),
    );
    const right = Array.from({ length: 10 }, (_, i) =>
      tb(`B${i}`, 100, 700 - i * 15),
    );
    const result = detectColumns([...left, ...right]);
    expect(result.columnCount).toBe(1);
  });

  it("does not split when one side has too few boxes", () => {
    const left = Array.from({ length: 15 }, (_, i) =>
      tb(`Main ${i}`, 72, 700 - i * 15),
    );
    const right = [tb("Margin note", 400, 600)]; // only 1 box on right
    const result = detectColumns([...left, ...right]);
    expect(result.columnCount).toBe(1);
  });
});
