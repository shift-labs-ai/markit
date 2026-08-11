/**
 * Running header/footer detection and removal.
 *
 * Many PDFs have repeated text at the top or bottom of every page:
 * document titles, chapter names, page numbers, copyright notices.
 * These pollute the markdown output as false headings or noise.
 *
 * Algorithm:
 *   1. For each page, bucket text boxes by Y position (top/bottom zones)
 *   2. Collect the text content at each zone across all pages
 *   3. Text appearing on >20% of pages OR 8+ consecutive pages is a
 *      running header/footer
 *   4. Remove matching text boxes before further processing
 */

import type { PageContent, TextBox } from "./types.js";

/** Minimum number of pages to enable header/footer detection. */
const MIN_PAGES = 5;

/** Fraction of page height treated as each running-margin zone. */
const MARGIN_ZONE_RATIO = 0.12;

function inMarginZone(midY: number, pageHeight: number): boolean {
  const margin = pageHeight * MARGIN_ZONE_RATIO;
  return midY >= pageHeight - margin || midY <= margin;
}

/**
 * Minimum consecutive pages a text must appear on to be considered a
 * running header/footer. Catches both document-wide headers (appearing
 * on every page) and chapter-specific headers (appearing on 4+ consecutive
 * pages within a chapter).
 */
const MIN_CONSECUTIVE_PAGES = 8;

/**
 * Detect and remove running headers and footers from all pages.
 * Mutates the pages array in place, removing header/footer text boxes.
 *
 * Uses two strategies:
 *   1. Global frequency: text appearing on > 20% of all pages
 *   2. Consecutive runs: text appearing on 8+ consecutive pages
 */
export function stripHeadersFooters(pages: PageContent[]): void {
  if (pages.length < MIN_PAGES) return;

  // Step 1: Build per-page zone text sets
  const pageZoneTexts: Array<Set<string>> = [];

  for (const page of pages) {
    const zoneTexts = new Set<string>();
    for (const tb of page.textBoxes) {
      const midY = (tb.bounds.top + tb.bounds.bottom) / 2;
      if (inMarginZone(midY, page.pageHeight)) {
        const key = tb.text.trim().replace(/\s+/g, " ");
        if (key.length > 0) zoneTexts.add(key);
      }
    }
    pageZoneTexts.push(zoneTexts);
  }

  // Step 2: Count global frequency AND longest consecutive run for each text
  const globalCount = new Map<string, number>();
  const maxConsecutive = new Map<string, number>();

  // Collect all unique zone texts
  const allTexts = new Set<string>();
  for (const zts of pageZoneTexts) {
    for (const t of zts) allTexts.add(t);
  }

  for (const text of allTexts) {
    let total = 0;
    let consecutive = 0;
    let maxRun = 0;

    for (const zts of pageZoneTexts) {
      if (zts.has(text)) {
        total++;
        consecutive++;
        if (consecutive > maxRun) maxRun = consecutive;
      } else {
        consecutive = 0;
      }
    }

    globalCount.set(text, total);
    maxConsecutive.set(text, maxRun);
  }

  // Step 3: Identify running headers/footers
  const globalThreshold = Math.max(3, Math.floor(pages.length * 0.2));
  const repeatedTexts = new Set<string>();

  for (const text of allTexts) {
    const gc = globalCount.get(text) ?? 0;
    const mc = maxConsecutive.get(text) ?? 0;

    // Global: appears on 20%+ of pages
    if (gc >= globalThreshold) {
      repeatedTexts.add(text);
      continue;
    }

    // Consecutive: appears on 8+ consecutive pages (chapter-level headers)
    if (mc >= MIN_CONSECUTIVE_PAGES) {
      repeatedTexts.add(text);
    }
  }

  if (repeatedTexts.size === 0) return;

  // Step 4: Remove matching text boxes from each page
  for (const page of pages) {
    page.textBoxes = page.textBoxes.filter((tb) => {
      const midY = (tb.bounds.top + tb.bounds.bottom) / 2;
      if (!inMarginZone(midY, page.pageHeight)) return true;

      const normalized = tb.text.trim().replace(/\s+/g, " ");
      return !repeatedTexts.has(normalized);
    });
  }
}

/**
 * Fraction of page height treated as the top/bottom band for the
 * single-page chrome detector. Wider than the repetition band: chrome on
 * a first page can sit a little deeper (e.g. a citation banner above a
 * paper title).
 */
const SP_BAND_RATIO = 0.15;

/**
 * Minimum gap, in multiples of the page's body font size, between a
 * chrome candidate and the nearest body-side line. Real chrome is
 * visually separated from content.
 */
const SP_ISOLATION_GAP_RATIO = 1.0;

/** Chrome candidates longer than this are body prose, never chrome. */
const SP_MAX_CHARS = 200;

/**
 * Maximum lines in a strippable chrome group. Running chrome is 1–4
 * short lines; a larger block is a title/abstract and must survive
 * even when a stray page number rides along.
 */
const SP_MAX_GROUP_LINES = 4;

/**
 * Does the text match an unambiguous running-chrome signature?
 * URLs/DOIs, journal-banner phrases, copyright marks, `Page N`,
 * standalone page numbers, volume/issue markers, and journal citation
 * lines (year + page range, short). False positives on body prose are
 * the main risk — every branch here must be unambiguous.
 */
export function matchesChromePattern(text: string): boolean {
  const t = text.trim();
  if (t.length === 0) return false;
  const lower = t.toLowerCase();

  // URL / DOI prefixes — chrome lines are very often a citation URL.
  if (
    lower.includes("http://") ||
    lower.includes("https://") ||
    lower.startsWith("www.") ||
    lower.includes(" www.") ||
    lower.includes("doi:") ||
    lower.includes("doi.org/") ||
    lower.includes("dx.doi.org")
  ) {
    return true;
  }

  // Common journal-paper banner phrases.
  if (
    lower.includes("please cite this article") ||
    lower.includes("contents lists available at") ||
    lower.includes("available online at") ||
    lower.includes("downloaded from")
  ) {
    return true;
  }

  // Copyright / trademark chrome.
  if (
    t.includes("\u00a9") ||
    lower.includes("copyright ") ||
    lower.includes("all rights reserved")
  ) {
    return true;
  }

  // "Page N" / "Page N of M".
  if (/^page \d/.test(lower)) return true;

  // Lone page number (≤4 digits) or roman-numeral folio.
  if (/^\d{1,4}$/.test(t)) return true;
  if (
    t.length >= 2 &&
    t.length <= 7 &&
    /^m{0,3}(cm|cd|d?c{0,3})(xc|xl|l?x{0,3})(ix|iv|v?i{0,3})$/.test(lower) &&
    lower.length > 0
  ) {
    return true;
  }

  // Volume markers ("Vol. 24" / "Volume 81" / "v. 19, n. 1") with a
  // digit later.
  if (/(?:^|[^a-z0-9])vol(?:ume)?[. ,].*\d/.test(lower)) return true;
  if (/(?:^|[^a-z0-9])[vn]\. ?\d/.test(lower)) return true;

  // Author running heads ("Tao et al.", "Martinez et al. Respiratory
  // Research (2019)") — short lines only.
  if (t.length <= 80 && lower.includes("et al")) return true;

  // Journal-cite style: a 4-digit year AND a numeric page range, short
  // enough to plausibly be chrome.
  if (t.length <= 120 && hasYear(lower) && hasDigitRange(t)) return true;

  return false;
}

function hasYear(lower: string): boolean {
  return /(?:^|[^a-z0-9])(?:19|20)\d\d(?![a-z0-9])/.test(lower);
}

/**
 * A digits–digits range that is not a `YYYY-MM`/`YYYY-MM-DD` calendar
 * date (a 4-digit 19xx/20xx group followed by a 1-2 digit group is a
 * date, not a page range).
 */
function hasDigitRange(t: string): boolean {
  for (const m of t.matchAll(/(\d+) *[-\u2013\u2014] *(\d+)/g)) {
    const g1 = m[1];
    const g2 = m[2];
    const g1IsYear =
      g1.length === 4 && (g1.startsWith("19") || g1.startsWith("20"));
    if (!(g1IsYear && g2.length <= 2)) return true;
  }
  return false;
}

/** Median font size of a page's text boxes — the body-size reference. */
function bodyFontSize(page: PageContent): number {
  const sizes = page.textBoxes
    .map((tb) => tb.fontSize)
    .filter((s) => s > 0)
    .sort((a, b) => a - b);
  if (sizes.length === 0) return 0;
  return sizes[Math.floor(sizes.length / 2)];
}

/**
 * Per-page chrome detection: position + isolation + pattern signature.
 *
 * Complements `stripHeadersFooters`: the repetition detector needs ≥5
 * pages, so single pages (and inconsistent per-page chrome) slip
 * through. A box is stripped when it sits fully inside the top or
 * bottom band, matches an unambiguous chrome pattern, is short, and is
 * separated from the nearest body-side non-chrome line by at least one
 * body-line height. Titles and headings never match the pattern gate,
 * so they survive regardless of position.
 *
 * Coordinates are PDF user space: Y grows upward, `bounds.top` is the
 * numerically larger edge.
 */
export function stripSinglePageChrome(pages: PageContent[]): void {
  for (const page of pages) {
    const h = page.pageHeight;
    if (h <= 0 || page.textBoxes.length === 0) continue;
    const topBandFloor = h * (1 - SP_BAND_RATIO);
    const bottomBandCeil = h * SP_BAND_RATIO;
    const bodySize = bodyFontSize(page);
    const requiredGap = SP_ISOLATION_GAP_RATIO * (bodySize > 0 ? bodySize : 10);

    // Band boxes chained into groups: consecutive lines (by Y) whose
    // inter-line gap is below the isolation gap belong together. A
    // running footer is often several lines (citation + page number);
    // one pattern hit licenses the whole group once the GROUP is
    // isolated from the body.
    const strip = new Set<string>();
    for (const band of ["top", "bottom"] as const) {
      const inBand = (tb: TextBox) =>
        band === "top"
          ? tb.bounds.bottom >= topBandFloor
          : tb.bounds.top <= bottomBandCeil;
      const bandBoxes = page.textBoxes
        .filter((tb) => inBand(tb) && tb.text.trim().length > 0)
        .sort((a, b) => b.bounds.top - a.bounds.top);
      if (bandBoxes.length === 0) continue;

      // Chain into groups.
      const groups: TextBox[][] = [];
      let cur: TextBox[] = [bandBoxes[0]];
      for (let i = 1; i < bandBoxes.length; i++) {
        const prev = cur[cur.length - 1];
        const gap = prev.bounds.bottom - bandBoxes[i].bounds.top;
        if (gap <= requiredGap) {
          cur.push(bandBoxes[i]);
        } else {
          groups.push(cur);
          cur = [bandBoxes[i]];
        }
      }
      groups.push(cur);

      for (const group of groups) {
        if (group.length > SP_MAX_GROUP_LINES) continue;
        // A group that is most of the page is content, not chrome.
        if (group.length * 2 > page.textBoxes.length) continue;
        if (group.some((tb) => tb.text.trim().length > SP_MAX_CHARS)) continue;
        if (!group.some((tb) => matchesChromePattern(tb.text.trim()))) continue;

        // Isolation: gap from the group's body-side edge to the nearest
        // line outside the group.
        const groupIds = new Set(group.map((tb) => tb.id));
        const edge =
          band === "top"
            ? Math.min(...group.map((tb) => tb.bounds.bottom))
            : Math.max(...group.map((tb) => tb.bounds.top));
        let nearest = Number.POSITIVE_INFINITY;
        for (const other of page.textBoxes) {
          if (groupIds.has(other.id)) continue;
          if (band === "top" && other.bounds.top < edge) {
            nearest = Math.min(nearest, edge - other.bounds.top);
          } else if (band === "bottom" && other.bounds.bottom > edge) {
            nearest = Math.min(nearest, other.bounds.bottom - edge);
          }
        }
        if (nearest < requiredGap) continue;

        for (const tb of group) strip.add(tb.id);
      }
    }

    if (strip.size > 0) {
      page.textBoxes = page.textBoxes.filter((tb) => !strip.has(tb.id));
    }
  }
}
