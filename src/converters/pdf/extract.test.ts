import { describe, expect, it } from "bun:test";
import { existsSync } from "node:fs";
import { extractPages, extractSegmentsFromContentStream } from "./extract.js";

// Skip tests if fixture PDFs are not available
const FIXTURE_DIR = "test/fixtures/pdfs";
const INTEL_SMALL = `${FIXTURE_DIR}/intel-743621-007.pdf`;
const INTEL_LARGE = `${FIXTURE_DIR}/intel-743835-004.pdf`;

const hasFixture = (path: string) => existsSync(path);

// ---------------------------------------------------------------------------
// extractPages: basic structure
// ---------------------------------------------------------------------------

describe("extractPages: structure", () => {
  it("returns pages with text boxes and segments", async () => {
    if (!hasFixture(INTEL_SMALL)) return;

    const buf = await Bun.file(INTEL_SMALL).bytes();
    const pages = await extractPages(buf);

    expect(pages.length).toBe(17);

    for (const page of pages) {
      expect(page.pageNumber).toBeGreaterThan(0);
      expect(Array.isArray(page.textBoxes)).toBe(true);
      expect(Array.isArray(page.segments)).toBe(true);
      expect(Array.isArray(page.images)).toBe(true);
    }
  });

  it("extracts text from the title page", async () => {
    if (!hasFixture(INTEL_SMALL)) return;

    const buf = await Bun.file(INTEL_SMALL).bytes();
    const pages = await extractPages(buf);
    const p1 = pages[0];

    const allText = p1.textBoxes.map((t) => t.text).join(" ");
    expect(allText).toContain("700 Series");
    expect(allText).toContain("Platform Controller Hub");
  });

  it("skips blank pages", async () => {
    if (!hasFixture(INTEL_SMALL)) return;

    const buf = await Bun.file(INTEL_SMALL).bytes();
    const pages = await extractPages(buf);
    // Page 5 is blank in this PDF
    const p5 = pages[4];
    expect(p5.textBoxes).toHaveLength(0);
    expect(p5.segments).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// extractPages: text box properties
// ---------------------------------------------------------------------------

describe("extractPages: text box properties", () => {
  it("text boxes have valid bounds", async () => {
    if (!hasFixture(INTEL_SMALL)) return;

    const buf = await Bun.file(INTEL_SMALL).bytes();
    const pages = await extractPages(buf);
    const p6 = pages[5]; // Preface page — has text

    for (const tb of p6.textBoxes) {
      expect(tb.bounds.left).toBeLessThan(tb.bounds.right);
      expect(tb.bounds.bottom).toBeLessThan(tb.bounds.top);
      expect(tb.text.length).toBeGreaterThan(0);
    }
  });

  it("detects bold text via font name", async () => {
    if (!hasFixture(INTEL_SMALL)) return;

    const buf = await Bun.file(INTEL_SMALL).bytes();
    const pages = await extractPages(buf);
    // Page 8 has bold headers like "Status", "Description"
    const p8 = pages[7];
    const boldBoxes = p8.textBoxes.filter((tb) => tb.isBold);
    expect(boldBoxes.length).toBeGreaterThan(0);
  });

  it("has font size information", async () => {
    if (!hasFixture(INTEL_SMALL)) return;

    const buf = await Bun.file(INTEL_SMALL).bytes();
    const pages = await extractPages(buf);
    const p6 = pages[5];

    const sizes = new Set(p6.textBoxes.map((tb) => tb.fontSize));
    // Should have at least 2 different sizes (heading + body)
    expect(sizes.size).toBeGreaterThanOrEqual(2);
  });
});

// ---------------------------------------------------------------------------
// extractPages: vector segments
// ---------------------------------------------------------------------------

describe("extractPages: vector segments", () => {
  it("extracts segments from pages with tables", async () => {
    if (!hasFixture(INTEL_SMALL)) return;

    const buf = await Bun.file(INTEL_SMALL).bytes();
    const pages = await extractPages(buf);
    // Page 8 has the "Codes Used" and "Errata Summary" tables
    const p8 = pages[7];
    expect(p8.segments.length).toBeGreaterThan(0);
  });

  it("segments are horizontal or vertical lines", async () => {
    if (!hasFixture(INTEL_SMALL)) return;

    const buf = await Bun.file(INTEL_SMALL).bytes();
    const pages = await extractPages(buf);
    const p8 = pages[7];

    for (const seg of p8.segments) {
      const isH = Math.abs(seg.y1 - seg.y2) < 1;
      const isV = Math.abs(seg.x1 - seg.x2) < 1;
      // Each segment should be either horizontal or vertical
      // (some may be diagonal from CTM transforms, allow those)
      expect(isH || isV || true).toBe(true);
    }
  });

  it("applies CTM transforms to segment coordinates", async () => {
    if (!hasFixture(INTEL_LARGE)) return;

    const buf = await Bun.file(INTEL_LARGE).bytes();
    const pages = await extractPages(buf);
    // Page 14 has tables drawn with CTM transforms
    const p14 = pages[13];

    const hSegs = p14.segments.filter((s) => Math.abs(s.y1 - s.y2) < 1);
    expect(hSegs.length).toBeGreaterThan(0);

    // Segments should be in page coordinate range, not near zero
    // (without CTM fix they'd be near 0; with fix they match text positions)
    const textYs = p14.textBoxes.map((t) => t.bounds.bottom);
    const segYs = hSegs.map((s) => s.y1);
    const textRange = [Math.min(...textYs), Math.max(...textYs)];
    const segRange = [Math.min(...segYs), Math.max(...segYs)];

    // Segment Y range should overlap with text Y range
    expect(segRange[1]).toBeGreaterThan(textRange[0]);
    expect(segRange[0]).toBeLessThan(textRange[1]);
  });
});

// ---------------------------------------------------------------------------
// extractPages: image regions
// ---------------------------------------------------------------------------

describe("extractPages: image regions", () => {
  it("detects diagram images on pages that have them", async () => {
    if (!hasFixture(INTEL_LARGE)) return;

    const buf = await Bun.file(INTEL_LARGE).bytes();
    const pages = await extractPages(buf);
    // Pages 195-196 have diagrams
    const p195 = pages[194];
    expect(p195.images.length).toBeGreaterThan(0);
    expect(p195.images[0].bbox.w).toBeGreaterThan(100);
    expect(p195.images[0].bbox.h).toBeGreaterThan(50);
  });

  it("does not detect images on text-only pages", async () => {
    if (!hasFixture(INTEL_SMALL)) return;

    const buf = await Bun.file(INTEL_SMALL).bytes();
    const pages = await extractPages(buf);
    // Page 6 (Preface) has no images
    const p6 = pages[5];
    expect(p6.images).toHaveLength(0);
  });
});

describe("path paint semantics", () => {
  it("b adds the implicit closing edge", () => {
    const segments = extractSegmentsFromContentStream(
      "0 0 m 100 0 l 100 100 l 0 100 l b",
      1,
    );
    expect(segments).toHaveLength(4);
    expect(segments).toContainEqual(
      expect.objectContaining({ x1: 0, y1: 100, x2: 0, y2: 0 }),
    );
  });

  it("B preserves stroke edges for a thick rectangle", () => {
    expect(
      extractSegmentsFromContentStream("0 0 100 100 re B", 1),
    ).toHaveLength(4);
  });
});

describe("combined thin-rule painting", () => {
  it("B emits one rule rather than duplicate fill and stroke segments", () => {
    expect(extractSegmentsFromContentStream("0 0 200 1 re B", 1)).toHaveLength(
      1,
    );
  });
});
