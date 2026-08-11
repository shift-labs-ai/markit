/**
 * Markdown rendering for PDF pages.
 *
 * Converts table grids and free text boxes into markdown, handling:
 * - Table grid → markdown table (`| col | col |`)
 * - Free text → paragraphs with heading detection (by font size)
 * - Content ordering (top-to-bottom via Y coordinate)
 * - Paragraph wrap merging (lines broken across PDF line boundaries)
 * - Page number removal
 *
 * Ported from @oharato/pdf2md-ts, stripped of CJK/TDnet-specific logic.
 */

import type { ContentBlock, TableGrid, TextBox } from "./types.js";

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/** Convert full-width ASCII characters (Ａ→A, ！→! etc.) to normal ASCII. */
function normalizeFullWidthAscii(text: string): string {
  return text.replace(/[！-～]/g, (ch) =>
    String.fromCharCode(ch.charCodeAt(0) - 0xfee0),
  );
}

function escapeFreeText(text: string): string {
  let escaped = text
    .replaceAll("\\", "\\\\")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("`", "\\`")
    .replaceAll("*", "\\*")
    .replaceAll("_", "\\_")
    .replaceAll("[", "\\[")
    .replaceAll("]", "\\]");
  escaped = escaped.replace(/^(\s{0,3})([#>+-])(?=\s)/, "$1\\$2");
  escaped = escaped.replace(/^(\s{0,3}\d+)\.(?=\s)/, "$1\\.");
  return escaped;
}

function escapePipes(text: string): string {
  return normalizeFullWidthAscii(text)
    .replaceAll("|", "\\|")
    .replaceAll("\n", "<br>");
}

function escapeTableHtml(text: string): string {
  return normalizeFullWidthAscii(text)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("\n", "<br>");
}

function renderSpanningTable(table: TableGrid): string {
  const covered = new Set<string>();
  const rows: string[] = [];
  for (let row = 0; row < table.rows; row++) {
    const cells = table.cells
      .filter((cell) => cell.row === row)
      .sort((a, b) => a.col - b.col);
    const rendered: string[] = [];
    for (const cell of cells) {
      if (covered.has(`${cell.row}:${cell.col}`)) continue;
      const rowSpan = Math.max(1, cell.rowSpan);
      const colSpan = Math.max(1, cell.colSpan);
      for (
        let r = cell.row;
        r < Math.min(table.rows, cell.row + rowSpan);
        r++
      ) {
        for (
          let c = cell.col;
          c < Math.min(table.cols, cell.col + colSpan);
          c++
        ) {
          if (r !== cell.row || c !== cell.col) covered.add(`${r}:${c}`);
        }
      }
      const tag = row === 0 ? "th" : "td";
      const attrs = [
        rowSpan > 1 ? ` rowspan="${rowSpan}"` : "",
        colSpan > 1 ? ` colspan="${colSpan}"` : "",
      ].join("");
      rendered.push(
        `<${tag}${attrs}>${escapeTableHtml(cell.text.trim())}</${tag}>`,
      );
    }
    rows.push(`<tr>${rendered.join("")}</tr>`);
  }
  return `<table>\n${rows.join("\n")}\n</table>`;
}

// ---------------------------------------------------------------------------
// Table rendering
// ---------------------------------------------------------------------------

/**
 * Render a TableGrid as a markdown table.
 */
export function renderTableToMarkdown(table: TableGrid): string {
  if (table.rows === 0 || table.cols === 0) return "";
  if (table.cells.some((cell) => cell.rowSpan > 1 || cell.colSpan > 1)) {
    return renderSpanningTable(table);
  }

  const matrix: string[][] = Array.from({ length: table.rows }, () =>
    Array.from({ length: table.cols }, () => ""),
  );

  for (const cell of table.cells) {
    if (cell.row < table.rows && cell.col < table.cols) {
      matrix[cell.row][cell.col] = escapePipes(cell.text.trim());
    }
  }

  const promoted = matrix;

  const header = `| ${promoted[0].join(" | ")} |`;
  const divider = `| ${Array.from({ length: promoted[0].length }, () => "---").join(" | ")} |`;
  const body = promoted
    .slice(1)
    .map((row) => `| ${row.join(" | ")} |`)
    .join("\n");

  return [header, divider, body].filter((l) => l.length > 0).join("\n");
}

// ---------------------------------------------------------------------------
// Free text rendering
// ---------------------------------------------------------------------------

/** Y tolerance for grouping text boxes onto the same visual line. */
const TEXT_LINE_Y_TOLERANCE = 3;

/** Minimum X gap between adjacent boxes to mark line as tabular. */
const TABULAR_X_GAP = 30;

interface TextLine {
  text: string;
  topY: number;
  fontSize: number;
  isBold: boolean;
  isTabular: boolean;
}

/**
 * Minimum font size (pts) to consider when computing the modal body font.
 * Tiny labels from diagrams, footnote markers, and superscripts are excluded
 * so they don't skew the modal toward small sizes.
 */
const MIN_BODY_FONT_SIZE = 7;

/**
 * Compute the most frequent font size among text boxes, ignoring very small
 * text that likely comes from diagrams, footnotes, or superscripts.
 */
function modalFontSize(textBoxes: TextBox[]): number {
  const counts = new Map<number, number>();
  for (const tb of textBoxes) {
    const size = Math.round((tb.fontSize ?? 0) * 10) / 10;
    if (size < MIN_BODY_FONT_SIZE) continue;
    counts.set(size, (counts.get(size) ?? 0) + 1);
  }
  let modal = 0;
  let maxCount = 0;
  for (const [size, count] of counts) {
    if (count > maxCount) {
      maxCount = count;
      modal = size;
    }
  }
  return modal;
}

/** Group free text boxes into horizontal lines, sorted top-to-bottom. */
function groupFreeTextIntoLines(textBoxes: TextBox[]): TextLine[] {
  if (textBoxes.length === 0) return [];

  const sorted = [...textBoxes].sort((a, b) => {
    const ya = (a.bounds.top + a.bounds.bottom) / 2;
    const yb = (b.bounds.top + b.bounds.bottom) / 2;
    const dy = yb - ya;
    if (Math.abs(dy) > TEXT_LINE_Y_TOLERANCE) return dy;
    return a.bounds.left - b.bounds.left;
  });

  const lines: TextLine[] = [];
  let curParts = [sorted[0].text];
  let curBoxes = [sorted[0]];
  let curY = (sorted[0].bounds.top + sorted[0].bounds.bottom) / 2;
  let curTopY = curY;
  let curFontSize = sorted[0].fontSize;
  let curIsBold = sorted[0].isBold;

  const finishLine = () => {
    let isTabular = false;
    for (let j = 1; j < curBoxes.length; j++) {
      if (
        curBoxes[j].bounds.left - curBoxes[j - 1].bounds.right >
        TABULAR_X_GAP
      ) {
        isTabular = true;
        break;
      }
    }
    lines.push({
      text: curParts.join(" "),
      topY: curTopY,
      fontSize: curFontSize,
      isBold: curIsBold,
      isTabular,
    });
  };

  for (let i = 1; i < sorted.length; i++) {
    const box = sorted[i];
    const cy = (box.bounds.top + box.bounds.bottom) / 2;
    if (Math.abs(cy - curY) <= TEXT_LINE_Y_TOLERANCE) {
      curParts.push(box.text);
      curBoxes.push(box);
      curFontSize = Math.max(curFontSize, box.fontSize);
      curIsBold = curIsBold || box.isBold;
    } else {
      finishLine();
      curParts = [box.text];
      curBoxes = [box];
      curY = cy;
      curTopY = cy;
      curFontSize = box.fontSize;
      curIsBold = box.isBold;
    }
  }
  finishLine();
  return lines;
}

/** Determine markdown heading prefix based on font size relative to body. */
function headingPrefix(
  fontSize: number,
  bodyFontSize: number,
  isBold: boolean,
): string {
  if (bodyFontSize <= 0) return "";

  const ratio = fontSize / bodyFontSize;

  // Large headings (>2x body size)
  if (ratio >= 2.0) return "# ";
  // Medium headings (~1.5x body size)
  if (ratio >= 1.4) return "## ";
  // Small headings (bold and slightly larger)
  if (ratio >= 1.1 && isBold) return "### ";

  return "";
}

// ---------------------------------------------------------------------------
// Block merging
// ---------------------------------------------------------------------------

/** Merge consecutive blocks with the same heading prefix (wrapped headings). */
function mergeConsecutiveHeadings(
  blocks: ContentBlock[],
  bodyFS: number,
): ContentBlock[] {
  if (blocks.length === 0) return [];

  const HEADING_RE = /^(#{1,6} )/;
  const maxGap = Math.max(bodyFS * 3, 30);
  const merged: ContentBlock[] = [];
  let cur = { ...blocks[0] };

  for (let i = 1; i < blocks.length; i++) {
    const next = blocks[i];
    const curMatch = cur.content.match(HEADING_RE);
    const nextMatch = next.content.match(HEADING_RE);
    const gap = cur.topY - next.topY;

    if (
      curMatch &&
      nextMatch &&
      curMatch[1] === nextMatch[1] &&
      gap <= maxGap
    ) {
      cur = {
        topY: cur.topY,
        content: `${cur.content} ${next.content.slice(nextMatch[1].length)}`,
        isTabular: cur.isTabular || next.isTabular,
      };
    } else {
      merged.push(cur);
      cur = { ...next };
    }
  }
  merged.push(cur);
  return merged;
}

/**
 * Merge consecutive plain-text blocks that are wrapped lines of the same paragraph.
 */
function mergeParagraphWraps(
  blocks: ContentBlock[],
  bodyFS: number,
): ContentBlock[] {
  if (blocks.length === 0 || bodyFS <= 0) return blocks;

  const HEADING_RE = /^#{1,6} /;
  const SENTENCE_END_RE = /[.!?…)\]]\s*$/;
  const maxGap = bodyFS * 2.0;
  const MIN_WRAP_LENGTH = 25;

  const merged: ContentBlock[] = [];
  let cur = { ...blocks[0], lastTopY: blocks[0].topY };

  for (let i = 1; i < blocks.length; i++) {
    const next = blocks[i];
    const curIsBody =
      !HEADING_RE.test(cur.content) && !cur.content.startsWith("|");
    const nextIsBody =
      !HEADING_RE.test(next.content) && !next.content.startsWith("|");
    const gap = cur.lastTopY - next.topY;

    const isWrap =
      curIsBody &&
      nextIsBody &&
      !cur.isTabular &&
      !next.isTabular &&
      gap > 0 &&
      gap <= maxGap &&
      cur.content.length > MIN_WRAP_LENGTH &&
      !SENTENCE_END_RE.test(cur.content);

    if (isWrap) {
      cur = {
        topY: cur.topY,
        lastTopY: next.topY,
        content: `${cur.content.trimEnd()} ${next.content.trimStart()}`,
        isTabular: false,
      };
    } else {
      merged.push({ topY: cur.topY, content: cur.content });
      cur = { ...next, lastTopY: next.topY };
    }
  }
  merged.push({ topY: cur.topY, content: cur.content });
  return merged;
}

/** Remove page number blocks near the bottom of the page. */
function removePageNumbers(
  blocks: ContentBlock[],
  pageNumber: number | undefined,
): ContentBlock[] {
  const PAGE_NUM_RE = /^(?:#{1,6}\s*)?\d+\s*$/;
  const BOTTOM_Y = 120;

  return blocks.filter((block, idx) => {
    const isBottom = idx >= blocks.length - 3;
    const isLowY = block.topY <= BOTTOM_Y;
    const match = block.content.trim().match(PAGE_NUM_RE);
    const isCurrentPage =
      pageNumber !== undefined &&
      match !== null &&
      Number(match[0].replace(/#/g, "").trim()) === pageNumber;
    return !(isBottom && isLowY && isCurrentPage);
  });
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Render one page's content: free text and tables interleaved top-to-bottom.
 */
export function renderPageContent(
  freeTextBoxes: TextBox[],
  tables: TableGrid[],
  imageBlocks: Array<{ topY: number; markdown: string }> = [],
  allTextBoxes?: TextBox[],
): string {
  const blocks: ContentBlock[] = [];
  // Use ALL text boxes (before table/diagram filtering) for modal font size,
  // so that diagram labels released as free text don't skew the body size.
  const bodyFS = modalFontSize(allTextBoxes ?? freeTextBoxes);

  // Free text lines
  for (const line of groupFreeTextIntoLines(freeTextBoxes)) {
    const prefix = headingPrefix(line.fontSize, bodyFS, line.isBold);
    blocks.push({
      topY: line.topY,
      content: prefix + escapeFreeText(line.text),
      isTabular: prefix === "" && line.isTabular,
    });
  }

  // Tables
  for (const table of tables) {
    const md = renderTableToMarkdown(table);
    if (md.length > 0) {
      blocks.push({ topY: table.topY, content: md });
    }
  }

  // Images
  for (const img of imageBlocks) {
    blocks.push({ topY: img.topY, content: img.markdown });
  }

  // Sort top-to-bottom (higher Y = higher on page = comes first)
  blocks.sort((a, b) => b.topY - a.topY);

  const pageNumber = freeTextBoxes[0]?.pageNumber ?? tables[0]?.pageNumber;
  const cleaned = removePageNumbers(blocks, pageNumber);
  const headingsMerged = mergeConsecutiveHeadings(cleaned, bodyFS);
  const merged = mergeParagraphWraps(headingsMerged, bodyFS);
  return merged
    .map((b) => b.content)
    .join("\n\n")
    .trim();
}
