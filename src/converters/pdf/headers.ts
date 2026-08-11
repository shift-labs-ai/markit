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

import type { PageContent } from "./types.js";

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
