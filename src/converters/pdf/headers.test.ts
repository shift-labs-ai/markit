import { describe, expect, it } from "bun:test";
import {
  matchesChromePattern,
  stripHeadersFooters,
  stripSinglePageChrome,
} from "./headers.js";
import type { PageContent, TextBox } from "./types.js";

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

/** Y-up box: `top` is the numerically larger edge. */
function box(id: string, text: string, top: number, bottom: number): TextBox {
  return {
    id,
    text,
    pageNumber: 1,
    fontSize: 10,
    isBold: false,
    bounds: { left: 50, right: 200, top, bottom },
  };
}

function singlePage(textBoxes: TextBox[]): PageContent {
  return {
    pageNumber: 1,
    pageWidth: 612,
    pageHeight: 792,
    segments: [],
    images: [],
    textBoxes,
  };
}

describe("matchesChromePattern", () => {
  it("recognizes unambiguous chrome signatures", () => {
    expect(matchesChromePattern("http://example.com/foo")).toBe(true);
    expect(matchesChromePattern("www.nature.com/scientificreports/")).toBe(
      true,
    );
    expect(matchesChromePattern("Please cite this article in press as:")).toBe(
      true,
    );
    expect(matchesChromePattern("Page 12 of 24")).toBe(true);
    expect(matchesChromePattern("9")).toBe(true);
    expect(matchesChromePattern("\u00a9 2023 Acme Corp")).toBe(true);
    expect(
      matchesChromePattern(
        "Cell Chemical Biology 24, 1\u20139, November 16, 2017",
      ),
    ).toBe(true);
  });

  it("never matches body prose or titles", () => {
    expect(
      matchesChromePattern("The quick brown fox jumps over the lazy dog."),
    ).toBe(false);
    expect(matchesChromePattern("Introduction")).toBe(false);
    // Year without a page range is a title, not chrome.
    expect(matchesChromePattern("Acme Annual Report 2023")).toBe(false);
    // YYYY-MM tracking dates are not page ranges.
    expect(matchesChromePattern("Company Tracking #: MS-2024-07")).toBe(false);
  });
});

describe("stripSinglePageChrome", () => {
  it("strips an isolated top-band URL on a single page", () => {
    // 792pt page: top band is y >= 673.2.
    const pages = [
      singlePage([
        box("chrome", "www.nature.com/scientificreports/", 780, 770),
        box("title", "Main Body Title", 600, 586),
        box("b1", "Body prose line one.", 570, 560),
      ]),
    ];
    stripSinglePageChrome(pages);
    expect(pages[0].textBoxes.map((t) => t.id)).toEqual(["title", "b1"]);
  });

  it("strips a bottom journal citation", () => {
    const pages = [
      singlePage([
        box("b1", "Body line.", 500, 490),
        box("b2", "More body.", 480, 470),
        box(
          "cite",
          "Cell Chemical Biology 24, 1\u20139, November 16, 2017",
          30,
          20,
        ),
      ]),
    ];
    stripSinglePageChrome(pages);
    expect(pages[0].textBoxes.map((t) => t.id)).toEqual(["b1", "b2"]);
  });

  it("preserves a title in the band without a chrome pattern", () => {
    const pages = [
      singlePage([
        box("title", "My Important Document", 770, 750),
        box("author", "Author Name", 730, 720),
        box("b1", "Body prose here.", 500, 490),
      ]),
    ];
    stripSinglePageChrome(pages);
    expect(pages[0].textBoxes.length).toBe(3);
  });

  it("keeps chrome-looking text that has no isolation gap", () => {
    const pages = [
      singlePage([
        box("url", "http://example.com/foo", 780, 770),
        // Next line 5pt below — well within one body-line height.
        box("b1", "Body line right after.", 765, 755),
        box("b2", "Continuing body.", 750, 740),
      ]),
    ];
    stripSinglePageChrome(pages);
    expect(pages[0].textBoxes.length).toBe(3);
  });
});
