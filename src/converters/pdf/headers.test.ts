import { describe, expect, it } from "bun:test";
import { stripHeadersFooters } from "./headers.js";
import type { PageContent } from "./types.js";

function page(pageNumber: number): PageContent {
  return {
    pageNumber,
    pageWidth: 300,
    pageHeight: 400,
    segments: [],
    images: [],
    textBoxes: [
      {
        id: `p${pageNumber}-header`,
        text: "Short-page header",
        pageNumber,
        fontSize: 10,
        isBold: false,
        bounds: { left: 20, right: 120, bottom: 360, top: 370 },
      },
      {
        id: `p${pageNumber}-body`,
        text: "Body",
        pageNumber,
        fontSize: 10,
        isBold: false,
        bounds: { left: 20, right: 120, bottom: 210, top: 220 },
      },
    ],
  };
}

describe("stripHeadersFooters", () => {
  it("uses page-relative zones on short pages", () => {
    const pages = Array.from({ length: 10 }, (_, i) => page(i + 1));
    stripHeadersFooters(pages);
    expect(
      pages.every((p) =>
        p.textBoxes.every((t) => t.text !== "Short-page header"),
      ),
    ).toBe(true);
    expect(pages.every((p) => p.textBoxes.some((t) => t.text === "Body"))).toBe(
      true,
    );
  });
});
