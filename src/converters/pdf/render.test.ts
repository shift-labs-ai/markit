import { beforeEach, describe, expect, it } from "bun:test";
import { renderPageContent, renderTableToMarkdown } from "./render.js";
import type { TableGrid, TextBox } from "./types.js";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

let _id = 0;

beforeEach(() => {
  _id = 0;
});

function box(
  text: string,
  opts: {
    x?: number;
    y?: number;
    w?: number;
    h?: number;
    fontSize?: number;
    isBold?: boolean;
  } = {},
): TextBox {
  const {
    x = 100,
    y = 500,
    w = 100,
    h = 10,
    fontSize = 9,
    isBold = false,
  } = opts;
  return {
    id: `t${_id++}`,
    text,
    pageNumber: 1,
    fontSize,
    isBold,
    bounds: { left: x, right: x + w, bottom: y, top: y + h },
  };
}

function makeGrid(overrides: Partial<TableGrid> = {}): TableGrid {
  return {
    pageNumber: 1,
    rows: 2,
    cols: 2,
    topY: 300,
    warnings: [],
    isBorderless: false,
    cells: [
      { row: 0, col: 0, text: "Name", rowSpan: 1, colSpan: 1 },
      { row: 0, col: 1, text: "Role", rowSpan: 1, colSpan: 1 },
      { row: 1, col: 0, text: "Alice", rowSpan: 1, colSpan: 1 },
      { row: 1, col: 1, text: "CEO", rowSpan: 1, colSpan: 1 },
    ],
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// renderTableToMarkdown
// ---------------------------------------------------------------------------

describe("renderTableToMarkdown", () => {
  it("renders a 2x2 table", () => {
    const md = renderTableToMarkdown(makeGrid());
    expect(md).toContain("| Name | Role |");
    expect(md).toContain("| --- | --- |");
    expect(md).toContain("| Alice | CEO |");
  });

  it("returns empty string for rows=0", () => {
    expect(renderTableToMarkdown(makeGrid({ rows: 0, cells: [] }))).toBe("");
  });

  it("returns empty string for cols=0", () => {
    expect(renderTableToMarkdown(makeGrid({ cols: 0, cells: [] }))).toBe("");
  });

  it("escapes pipe characters in cell text", () => {
    const g = makeGrid({
      rows: 1,
      cols: 1,
      cells: [{ row: 0, col: 0, text: "A|B", rowSpan: 1, colSpan: 1 }],
    });
    expect(renderTableToMarkdown(g)).toContain("A\\|B");
  });

  it("converts newlines to <br>", () => {
    const g = makeGrid({
      rows: 1,
      cols: 1,
      cells: [{ row: 0, col: 0, text: "line1\nline2", rowSpan: 1, colSpan: 1 }],
    });
    expect(renderTableToMarkdown(g)).toContain("line1<br>line2");
  });

  it("normalizes full-width ASCII characters", () => {
    const g = makeGrid({
      rows: 1,
      cols: 1,
      cells: [{ row: 0, col: 0, text: "Ａ＋Ｂ", rowSpan: 1, colSpan: 1 }],
    });
    expect(renderTableToMarkdown(g)).toContain("A+B");
  });

  it("does not move or delete legitimate sparse columns", () => {
    const cells = [
      ["H0", "", "H2", "", "H4", "H5"],
      ["A", "keep-1", "", "keep-3", "", "E"],
      ["B", "", "C", "", "F", "G"],
    ].flatMap((row, r) =>
      row.map((text, c) => ({
        row: r,
        col: c,
        text,
        rowSpan: 1,
        colSpan: 1,
      })),
    );
    const md = renderTableToMarkdown(makeGrid({ rows: 3, cols: 6, cells }));
    expect(md).toContain("| H0 |  | H2 |  | H4 | H5 |");
    expect(md).toContain("| A | keep-1 |  | keep-3 |  | E |");
  });

  it("renders merged cells with HTML span attributes", () => {
    const table: TableGrid = {
      pageNumber: 1,
      topY: 100,
      rows: 2,
      cols: 2,
      cells: [
        { row: 0, col: 0, text: "A&B", rowSpan: 2, colSpan: 1 },
        { row: 0, col: 1, text: "Header", rowSpan: 1, colSpan: 1 },
        { row: 1, col: 1, text: "Value", rowSpan: 1, colSpan: 1 },
      ],
      warnings: [],
      isBorderless: false,
    };
    expect(renderTableToMarkdown(table)).toContain(
      '<th rowspan="2">A&amp;B</th>',
    );
  });

  it("renders a single-row table (header only)", () => {
    const g = makeGrid({
      rows: 1,
      cols: 2,
      cells: [
        { row: 0, col: 0, text: "Col A", rowSpan: 1, colSpan: 1 },
        { row: 0, col: 1, text: "Col B", rowSpan: 1, colSpan: 1 },
      ],
    });
    const md = renderTableToMarkdown(g);
    expect(md).toContain("| Col A | Col B |");
    expect(md).toContain("| --- | --- |");
  });
});

// ---------------------------------------------------------------------------
// renderPageContent: free text
// ---------------------------------------------------------------------------

describe("renderPageContent: free text", () => {
  it("outputs plain text", () => {
    const result = renderPageContent([box("Hello world")], []);
    expect(result).toContain("Hello world");
  });

  it("escapes literal markdown and HTML in extracted text", () => {
    const result = renderPageContent(
      [box("# literal", { y: 600 }), box("*not emphasis* <tag>", { y: 500 })],
      [],
    );
    expect(result).toContain("\\# literal");
    expect(result).toContain("\\*not emphasis\\* &lt;tag&gt;");
  });

  it("merges text boxes on the same Y line", () => {
    const boxes = [
      box("first", { x: 100, y: 500 }),
      box("second", { x: 220, y: 501 }), // Y diff=1 → same line
    ];
    const result = renderPageContent(boxes, []);
    expect(result).toContain("first second");
  });

  it("separates text boxes on different Y lines", () => {
    const boxes = [box("line one", { y: 600 }), box("line two", { y: 500 })];
    const result = renderPageContent(boxes, []);
    expect(result).toContain("line one");
    expect(result).toContain("line two");
    // line one should come before line two (higher Y = earlier)
    expect(result.indexOf("line one")).toBeLessThan(result.indexOf("line two"));
  });
});

// ---------------------------------------------------------------------------
// renderPageContent: heading detection
// ---------------------------------------------------------------------------

describe("renderPageContent: headings", () => {
  it("large font becomes # heading", () => {
    const boxes = [
      box("Body text", { y: 400, fontSize: 9 }),
      box("Big Title", { y: 600, fontSize: 20 }),
    ];
    const result = renderPageContent(boxes, []);
    expect(result).toContain("# Big Title");
  });

  it("medium font becomes ## heading", () => {
    const boxes = [
      box("Body text", { y: 400, fontSize: 9 }),
      box("Section", { y: 600, fontSize: 14 }),
    ];
    const result = renderPageContent(boxes, []);
    expect(result).toContain("## Section");
  });

  it("bold slightly-larger font becomes ### heading", () => {
    const boxes = [
      box("Body text", { y: 400, fontSize: 9 }),
      box("Subsection", { y: 600, fontSize: 11, isBold: true }),
    ];
    const result = renderPageContent(boxes, []);
    expect(result).toContain("### Subsection");
  });

  it("different heading levels do not merge", () => {
    const boxes = [
      box("Body 1", { y: 200, fontSize: 9 }),
      box("Body 2", { y: 180, fontSize: 9 }),
      box("Body 3", { y: 160, fontSize: 9 }),
      box("Chapter Title", { y: 700, fontSize: 20 }), // # (ratio 2.2)
      box("Section Title", { y: 670, fontSize: 14 }), // ## (ratio 1.5)
    ];
    const result = renderPageContent(boxes, []);
    expect(result).toContain("# Chapter Title");
    expect(result).toContain("## Section Title");
    // They should be separate headings, not merged
    const h1Count = (result.match(/^# /gm) ?? []).length;
    const h2Count = (result.match(/^## /gm) ?? []).length;
    expect(h1Count).toBe(1);
    expect(h2Count).toBe(1);
  });

  it("same-size text does not become a heading", () => {
    const boxes = [
      box("Regular A", { y: 600, fontSize: 9 }),
      box("Regular B", { y: 500, fontSize: 9 }),
    ];
    const result = renderPageContent(boxes, []);
    expect(result).not.toMatch(/^#/m);
  });

  it("merges consecutive same-level headings (wrapped title)", () => {
    // Need enough body-sized boxes to establish modal font size
    const boxes = [
      box("Body line 1", { y: 300, fontSize: 9 }),
      box("Body line 2", { y: 280, fontSize: 9 }),
      box("Body line 3", { y: 260, fontSize: 9 }),
      box("Long Title Part One", { y: 620, fontSize: 20 }),
      box("Part Two of Title", { y: 605, fontSize: 20 }),
    ];
    const result = renderPageContent(boxes, []);
    // Both parts should be in a single # heading, joined with a space
    expect(result).toContain("# Long Title Part One Part Two of Title");
    const headingCount = (result.match(/^# /gm) ?? []).length;
    expect(headingCount).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// renderPageContent: tables interleaved with text
// ---------------------------------------------------------------------------

describe("renderPageContent: text + tables ordering", () => {
  it("orders text and tables by Y position (top to bottom)", () => {
    const title = box("Title", { y: 700, fontSize: 20 });
    const g = makeGrid({ topY: 300 });
    const result = renderPageContent([title], [g]);

    const titlePos = result.indexOf("Title");
    const tablePos = result.indexOf("| Name |");
    expect(titlePos).toBeLessThan(tablePos);
  });

  it("includes both text and table content", () => {
    const result = renderPageContent(
      [box("Some text", { y: 600 })],
      [makeGrid()],
    );
    expect(result).toContain("Some text");
    expect(result).toContain("| Name | Role |");
  });
});

// ---------------------------------------------------------------------------
// renderPageContent: image blocks
// ---------------------------------------------------------------------------

describe("renderPageContent: image blocks", () => {
  it("includes image markdown at correct position", () => {
    const title = box("Section Title", { y: 700, fontSize: 20 });
    const body = box("Body text below", { y: 300 });
    const imageBlocks = [
      { topY: 500, markdown: "![diagram](images/fig1.png)" },
    ];
    const result = renderPageContent([title, body], [], imageBlocks);

    expect(result).toContain("![diagram](images/fig1.png)");
    // Image should be between title and body
    const titlePos = result.indexOf("Section Title");
    const imgPos = result.indexOf("![diagram]");
    const bodyPos = result.indexOf("Body text below");
    expect(titlePos).toBeLessThan(imgPos);
    expect(imgPos).toBeLessThan(bodyPos);
  });

  it("includes HTML comment placeholders", () => {
    const imageBlocks = [
      {
        topY: 500,
        markdown: "<!-- image: p5-img0 (page 5, 400x200pt) -->",
      },
    ];
    const result = renderPageContent([], [], imageBlocks);
    expect(result).toContain("<!-- image: p5-img0");
  });
});

// ---------------------------------------------------------------------------
// renderPageContent: page number removal
// ---------------------------------------------------------------------------

describe("renderPageContent: page number removal", () => {
  it("removes standalone page numbers at the bottom", () => {
    const boxes = [
      box("Real content", { y: 500 }),
      box("1", { y: 50 }), // matches the current page number
    ];
    const result = renderPageContent(boxes, []);
    expect(result).toContain("Real content");
    expect(result).not.toMatch(/\b1\b/);
  });

  it("keeps a different standalone number near the bottom", () => {
    const result = renderPageContent(
      [box("Real content", { y: 500 }), box("42", { y: 50 })],
      [],
    );
    expect(result).toContain("42");
  });

  it("does not remove numbers that are part of content", () => {
    const boxes = [box("There are 42 items", { y: 500 })];
    const result = renderPageContent(boxes, []);
    expect(result).toContain("42");
  });
});
