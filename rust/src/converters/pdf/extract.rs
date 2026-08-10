//! PDF content extraction using mupdf.
//!
//! Extracts text boxes (with position, font size, bold) and vector line
//! segments (table borders) from each page. Uses mupdf's native engine
//! for fast parsing, and reads raw content streams for vector graphics.
//!
//! Coordinate system: PDF native (origin = bottom-left, Y increases upward).

use anyhow::{anyhow, Result};
use mupdf::pdf::PdfDocument;
use mupdf::{Document, TextPageFlags};

use super::types::{Bounds, ImageRegion, PageContent, Rect, Segment, TextBox};

// ---------------------------------------------------------------------------
// Text extraction
// ---------------------------------------------------------------------------

/// Fallback diagnostics, visible under MARKIT_DEBUG.
fn log_fallback(e: &anyhow::Error) {
    if std::env::var("MARKIT_DEBUG").is_ok() {
        eprintln!("[markit] pdf fast path fell back to mupdf: {e}");
    }
}

/// Disable ICC color management on this thread's MuPDF context, once.
fn disable_icc_once() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    DONE.with(|done| {
        if !done.get() {
            mupdf::Context::get().disable_icc();
            done.set(true);
        }
    });
}

/// Y tolerance for merging text fragments on the same visual line.
const SAME_LINE_Y_TOLERANCE: f64 = 2.0;
/// Max horizontal gap (pts) to merge adjacent fragments into one text box.
const MAX_MERGE_GAP: f64 = 14.0;

#[derive(Debug, Clone)]
struct RawTextItem {
    text: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    font_size: f64,
    is_bold: bool,
}

/// Superscripts and subscripts sit outside the flat Y band: a footnote
/// marker rides ~0.33em above the baseline of its line. MuPDF's stext
/// joins these into the line ("evaluation1"); match that by accepting a
/// clearly smaller item whose vertical span still overlaps most of the
/// other's. Same-size items keep the strict band, so ordinary adjacent
/// lines never fuse.
fn script_same_line(a: &RawTextItem, b: &RawTextItem) -> bool {
    let (small, large) = if a.font_size <= b.font_size {
        (a, b)
    } else {
        (b, a)
    };
    if large.font_size <= 0.0 || small.font_size > large.font_size * 0.8 {
        return false;
    }
    let overlap = (small.y + small.height).min(large.y + large.height) - small.y.max(large.y);
    overlap >= 0.5 * small.height.min(large.height).max(1.0)
}

/// Merge horizontally adjacent raw text items on the same visual line into
/// word/phrase-level text boxes.
fn merge_into_words(raws: &[RawTextItem]) -> Vec<RawTextItem> {
    if raws.is_empty() {
        return Vec::new();
    }

    // Sort by Y descending (top-first in bottom-left coords), then X ascending.
    // The tolerance-band comparator is intentionally NOT a total order (same
    // as the TS version): items within SAME_LINE_Y_TOLERANCE compare by X even
    // when their transitive Y chain drifts past the tolerance. JS Array#sort
    // tolerates inconsistent comparators; Rust's driftsort panics on them, so
    // we use a stable insertion sort that accepts the comparator as-is.
    let cmp = |a: &RawTextItem, b: &RawTextItem| {
        let dy = b.y - a.y;
        if dy.abs() > SAME_LINE_Y_TOLERANCE && !script_same_line(a, b) {
            dy.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
        }
    };
    let mut sorted: Vec<RawTextItem> = raws.to_vec();
    super::js_stable_sort(&mut sorted, cmp);

    let mut merged: Vec<RawTextItem> = Vec::new();
    let mut cur = sorted[0].clone();

    for next in sorted.iter().skip(1) {
        let same_y =
            (next.y - cur.y).abs() <= SAME_LINE_Y_TOLERANCE || script_same_line(&cur, next);
        let close = next.x <= cur.x + cur.width + MAX_MERGE_GAP;

        if same_y && close {
            let gap = next.x - (cur.x + cur.width);
            let sep = if gap > 1.0 { " " } else { "" };
            cur.text = format!("{}{}{}", cur.text, sep, next.text);
            cur.width = next.x + next.width - cur.x;
            cur.height = cur.height.max(next.height);
            cur.font_size = cur.font_size.max(next.font_size);
            cur.is_bold = cur.is_bold || next.is_bold;
        } else {
            merged.push(cur);
            cur = next.clone();
        }
    }
    merged.push(cur);
    merged
}

/// Extract text boxes from a mupdf page using structured text output.
///
/// mupdf's structured text uses top-left origin; we convert to
/// bottom-left (standard PDF coordinates) using the page height.
/// JSON structured-text shapes emitted by mupdf's C serializer
/// (fz_print_stext_page_as_json) — the same path the TS version consumes via
/// StructuredText.asJSON(). All coordinates and the font size are C
/// "(int)"-truncated by that serializer, which is exactly the precision the
/// TS pipeline sees.
#[cfg(test)]
#[derive(serde::Deserialize)]
struct StextJson {
    blocks: Vec<StextBlock>,
}

#[cfg(test)]
#[derive(serde::Deserialize)]
struct StextBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    lines: Vec<StextLine>,
}

#[cfg(test)]
#[derive(serde::Deserialize)]
struct StextLine {
    bbox: StextBbox,
    #[serde(default)]
    font: Option<StextFont>,
    #[serde(default)]
    text: String,
}

#[cfg(test)]
#[derive(serde::Deserialize)]
struct StextBbox {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[cfg(test)]
#[derive(serde::Deserialize)]
struct StextFont {
    #[serde(default)]
    name: String,
    #[serde(default)]
    weight: String,
    #[serde(default)]
    size: f64,
}

/// One stext device run serving both text boxes and image regions.
///
/// The TS pipeline ran the device twice (text-only flags, then with
/// preserve-images). Text output is line-based and image emission only
/// affects block partitioning, so a single combined-flag run yields
/// byte-identical results at half the device cost — pinned by
/// combined_flags_match_split_runs on the fixture corpus.
fn extract_page_content(
    page: &mupdf::Page,
    page_number: u32,
    page_height: f64,
) -> (Vec<TextBox>, Vec<ImageRegion>) {
    let flags = TextPageFlags::PRESERVE_WHITESPACE | TextPageFlags::PRESERVE_IMAGES;
    let Ok(text_page) = page.to_text_page(flags) else {
        return (Vec::new(), Vec::new());
    };

    let text_boxes =
        text_boxes_from_text_page(&text_page, page_number, page_height).unwrap_or_default();

    let bboxes: Vec<(f32, f32, f32, f32)> = text_page
        .blocks()
        .filter(|b| b.r#type() == mupdf::text_page::TextBlockType::Image)
        .map(|b| {
            let r = b.bounds();
            (r.x0, r.y0, r.x1, r.y1)
        })
        .collect();
    let images = image_regions_from_bboxes(&bboxes, page_number, page_height);

    (text_boxes, images)
}

#[cfg(test)]
fn extract_text_boxes(
    page: &mupdf::Page,
    page_number: u32,
    page_height: f64,
) -> Result<Vec<TextBox>> {
    // TS: page.toStructuredText("preserve-whitespace").asJSON(), then JSON
    // parsing. This walks the stext structs natively instead, replicating
    // mupdf's as_json serializer value-for-value (stext-output.c): C
    // "(int)"-truncated bbox and size, the first char's font per line, and
    // the raw char concatenation the JSON escape/unescape round-trip
    // preserves. parse_text_boxes_json remains as the differential-test
    // reference for this equivalence.
    extract_text_boxes_flags(
        page,
        page_number,
        page_height,
        TextPageFlags::PRESERVE_WHITESPACE,
    )
}

/// Text-box extraction with explicit stext flags (test reference).
#[cfg(test)]
fn extract_text_boxes_flags(
    page: &mupdf::Page,
    page_number: u32,
    page_height: f64,
    flags: TextPageFlags,
) -> Result<Vec<TextBox>> {
    let text_page = page
        .to_text_page(flags)
        .map_err(|e| anyhow!("Failed to extract text: {}", e))?;
    text_boxes_from_text_page(&text_page, page_number, page_height)
}

/// Native stext walk over an existing text page (see extract_text_boxes'
/// serializer-equivalence notes).
fn text_boxes_from_text_page(
    text_page: &mupdf::TextPage,
    page_number: u32,
    page_height: f64,
) -> Result<Vec<TextBox>> {
    let mut raws: Vec<RawTextItem> = Vec::new();

    for block in text_page.blocks() {
        if block.r#type() != mupdf::text_page::TextBlockType::Text {
            continue;
        }
        for line in block.lines() {
            let mut text = String::new();
            let mut first_char: Option<(f32, Option<mupdf::Font>)> = None;
            for ch in line.chars() {
                if first_char.is_none() {
                    first_char = Some((ch.size(), ch.font()));
                }
                if let Some(c) = ch.char() {
                    text.push(c);
                }
            }
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }

            // as_json: font info comes from the line's first char.
            let (font_size, is_bold) = match &first_char {
                Some((size, font)) => {
                    let size = (*size as i32) as f64;
                    let (weight_bold, name_lower) = match font {
                        Some(f) => (f.is_bold(), f.name().to_lowercase()),
                        None => (false, String::new()),
                    };
                    let is_bold = weight_bold
                        || name_lower.contains("bold")
                        || name_lower.contains("black")
                        || name_lower.contains("heavy");
                    (size, is_bold)
                }
                None => (0.0, false),
            };

            // as_json bbox: x/y/w/h are (int)-truncated from the f32 rect.
            let b = line.bounds();
            let x = (b.x0 as i32) as f64;
            let y = (b.y0 as i32) as f64;
            let w = ((b.x1 - b.x0) as i32) as f64;
            let h = ((b.y1 - b.y0) as i32) as f64;

            // Convert top-left to bottom-left: pdf_y = page_height - (y + h)
            let pdf_y = page_height - (y + h);

            raws.push(RawTextItem {
                text: trimmed.to_string(),
                x,
                y: pdf_y,
                width: w,
                height: h,
                font_size,
                is_bold,
            });
        }
    }

    finish_text_boxes(raws, page_number)
}

/// JSON-path equivalent of extract_text_boxes (differential-test reference).
#[cfg(test)]
fn extract_text_boxes_via_json(
    page: &mupdf::Page,
    page_number: u32,
    page_height: f64,
) -> Result<Vec<TextBox>> {
    let flags = TextPageFlags::PRESERVE_WHITESPACE;
    let text_page = page
        .to_text_page(flags)
        .map_err(|e| anyhow!("Failed to extract text: {}", e))?;
    let json = text_page
        .to_json(1.0)
        .map_err(|e| anyhow!("Failed to serialize text: {}", e))?;
    parse_text_boxes_json(&json, page_number, page_height)
}

/// Shared JSON → text-box parsing, used by both the sequential (safe API)
/// and parallel (independent-context FFI) paths. The JSON is produced by
/// the same mupdf serializer in both, so parsing here keeps them identical.
#[cfg(test)]
fn parse_text_boxes_json(json: &str, page_number: u32, page_height: f64) -> Result<Vec<TextBox>> {
    let stext: StextJson = serde_json::from_str(json)
        .map_err(|e| anyhow!("Failed to parse structured text JSON: {}", e))?;

    let mut raws: Vec<RawTextItem> = Vec::new();

    for block in &stext.blocks {
        if block.kind != "text" {
            continue;
        }
        for line in &block.lines {
            let text = line.text.trim();
            if text.is_empty() {
                continue;
            }

            let font_size = line.font.as_ref().map(|f| f.size).unwrap_or(0.0);
            let weight = line
                .font
                .as_ref()
                .map(|f| f.weight.as_str())
                .unwrap_or("normal");
            let font_name = line.font.as_ref().map(|f| f.name.as_str()).unwrap_or("");
            let lower_name = font_name.to_lowercase();
            let is_bold = weight == "bold"
                || lower_name.contains("bold")
                || lower_name.contains("black")
                || lower_name.contains("heavy");

            // mupdf bbox: {x, y, w, h} in top-left coords.
            // Convert to bottom-left: pdf_y = page_height - (bbox.y + bbox.h)
            let pdf_y = page_height - (line.bbox.y + line.bbox.h);

            raws.push(RawTextItem {
                text: text.to_string(),
                x: line.bbox.x,
                y: pdf_y,
                width: line.bbox.w,
                height: line.bbox.h,
                font_size,
                is_bold,
            });
        }
    }

    finish_text_boxes(raws, page_number)
}

/// Public shims for the fast extraction engine (fast_extract.rs), which
/// feeds the same downstream pipeline.
pub(crate) struct RawTextItemPub {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub font_size: f64,
    pub is_bold: bool,
}

pub(crate) fn finish_text_boxes_pub(
    raws: Vec<RawTextItemPub>,
    page_number: u32,
) -> Result<Vec<TextBox>> {
    let raws: Vec<RawTextItem> = raws
        .into_iter()
        .map(|r| RawTextItem {
            text: r.text,
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
            font_size: r.font_size,
            is_bold: r.is_bold,
        })
        .collect();
    finish_text_boxes(raws, page_number)
}

pub(crate) fn image_regions_from_bboxes_pub(
    bboxes: &[(f32, f32, f32, f32)],
    page_number: u32,
    page_height: f64,
) -> Vec<ImageRegion> {
    image_regions_from_bboxes(bboxes, page_number, page_height)
}

pub(crate) fn thin_rect_to_segment_pub(
    id: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Option<Segment> {
    thin_rect_to_segment(id, x, y, w, h)
}

pub(crate) fn push_stroked_rect_edges_pub(
    segments: &mut Vec<Segment>,
    id: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) {
    push_stroked_rect_edges(segments, id, x, y, w, h)
}

/// Shared tail of text-box extraction: word merging and TextBox assembly.
fn finish_text_boxes(raws: Vec<RawTextItem>, page_number: u32) -> Result<Vec<TextBox>> {
    let words = merge_into_words(&raws);

    Ok(words
        .into_iter()
        .enumerate()
        .map(|(i, w)| TextBox {
            id: format!("p{}-t{}", page_number, i),
            // RTL script arrives in visual order: restore logical order
            // (and base Arabic forms) at line level.
            text: match super::bidi::fix_rtl(&w.text) {
                Some(t) => t.trim().to_string(),
                None => w.text.trim().to_string(),
            },
            page_number,
            font_size: w.font_size,
            is_bold: w.is_bold,
            bounds: Bounds {
                left: w.x,
                right: w.x + w.width,
                bottom: w.y,
                top: w.y + w.height,
            },
        })
        .filter(|b| !b.text.is_empty())
        .collect())
}

/// Count pages where text boxes differ between text-only and combined
/// text+image stext flags (test reference for extract_page_content).
#[cfg(test)]
fn probe_flags_equivalence(input: &[u8]) -> Result<(i32, i32)> {
    let doc = Document::from_bytes(input, "application/pdf").map_err(|e| anyhow!("open: {e}"))?;
    let n = doc.page_count().map_err(|e| anyhow!("count: {e}"))?;
    let mut diffs = 0;
    for i in 0..n {
        let page = doc.load_page(i).map_err(|e| anyhow!("load: {e}"))?;
        let bounds = page.bounds().map_err(|e| anyhow!("bounds: {e}"))?;
        let h = (bounds.y1 - bounds.y0) as f64;
        let a = extract_text_boxes(&page, (i + 1) as u32, h).unwrap_or_default();
        let b = extract_text_boxes_flags(
            &page,
            (i + 1) as u32,
            h,
            TextPageFlags::PRESERVE_WHITESPACE | TextPageFlags::PRESERVE_IMAGES,
        )
        .unwrap_or_default();
        if format!("{a:?}") != format!("{b:?}") {
            diffs += 1;
        }
    }
    Ok((n, diffs))
}

/// Minimum aspect ratio for a filled rect to be considered a line.
const LINE_ASPECT_THRESHOLD: f64 = 6.0;
/// Minimum length (pts) for a segment to count.
const MIN_LENGTH: f64 = 2.0;
/// Maximum thickness (pts) for a border line (filters out filled areas).
const MAX_THICKNESS: f64 = 3.0;

/// Convert a thin filled rectangle to a horizontal or vertical segment.
fn thin_rect_to_segment(id: String, x: f64, y: f64, w: f64, h: f64) -> Option<Segment> {
    let aw = w.abs();
    let ah = h.abs();

    if aw > ah * LINE_ASPECT_THRESHOLD && aw >= MIN_LENGTH && ah <= MAX_THICKNESS {
        let cy = y + ah / 2.0;
        return Some(Segment {
            id,
            x1: x,
            y1: cy,
            x2: x + aw,
            y2: cy,
        });
    }

    if ah > aw * LINE_ASPECT_THRESHOLD && ah >= MIN_LENGTH && aw <= MAX_THICKNESS {
        let cx = x + aw / 2.0;
        return Some(Segment {
            id,
            x1: cx,
            y1: y,
            x2: cx,
            y2: y + ah,
        });
    }

    None
}

/// Emit 4 edge segments from a stroked rectangle.
fn push_stroked_rect_edges(segments: &mut Vec<Segment>, id: &str, x: f64, y: f64, w: f64, h: f64) {
    let aw = w.abs();
    let ah = h.abs();

    if aw >= MIN_LENGTH {
        segments.push(Segment {
            id: format!("{}-b", id),
            x1: x,
            y1: y,
            x2: x + aw,
            y2: y,
        });
        segments.push(Segment {
            id: format!("{}-t", id),
            x1: x,
            y1: y + ah,
            x2: x + aw,
            y2: y + ah,
        });
    }
    if ah >= MIN_LENGTH {
        segments.push(Segment {
            id: format!("{}-l", id),
            x1: x,
            y1: y,
            x2: x,
            y2: y + ah,
        });
        segments.push(Segment {
            id: format!("{}-r", id),
            x1: x + aw,
            y1: y,
            x2: x + aw,
            y2: y + ah,
        });
    }
}

// ---------------------------------------------------------------------------
// CTM (Current Transformation Matrix)
// ---------------------------------------------------------------------------

type CTM = [f64; 6];
const CTM_IDENTITY: CTM = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn ctm_concat(p: &CTM, c: &CTM) -> CTM {
    [
        p[0] * c[0] + p[2] * c[1],
        p[1] * c[0] + p[3] * c[1],
        p[0] * c[2] + p[2] * c[3],
        p[1] * c[2] + p[3] * c[3],
        p[0] * c[4] + p[2] * c[5] + p[4],
        p[1] * c[4] + p[3] * c[5] + p[5],
    ]
}

fn ctm_apply(m: &CTM, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

// ---------------------------------------------------------------------------
// Content stream parsing
// ---------------------------------------------------------------------------

fn extract_segments_from_content_stream(raw: &str, page_number: u32) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let tokens = tokenize_content_stream(raw);
    let mut idx: usize = 0;
    let mut stroke_width: f64 = 1.0;

    let mut ctm: CTM = CTM_IDENTITY;
    let mut state_stack: Vec<(CTM, f64)> = Vec::new();

    let mut cur_x: f64 = 0.0;
    let mut cur_y: f64 = 0.0;
    let mut path_start_x: f64 = 0.0;
    let mut path_start_y: f64 = 0.0;

    struct PRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    struct PLine {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    }

    let mut pending_rects: Vec<PRect> = Vec::new();
    let mut pending_lines: Vec<PLine> = Vec::new();

    #[inline]
    fn flush_path(
        mode: &str,
        page_number: u32,
        segments: &mut Vec<Segment>,
        pending_rects: &mut Vec<PRect>,
        pending_lines: &mut Vec<PLine>,
        ctm: &CTM,
        stroke_width: f64,
    ) {
        if mode == "fill" {
            for r in pending_rects.drain(..) {
                let (x0, y0) = ctm_apply(ctm, r.x, r.y);
                let (x1, y1) = ctm_apply(ctm, r.x + r.w, r.y + r.h);
                let id = format!("p{}-s{}", page_number, segments.len());
                if let Some(seg) = thin_rect_to_segment(
                    id,
                    x0.min(x1),
                    y0.min(y1),
                    (x1 - x0).abs(),
                    (y1 - y0).abs(),
                ) {
                    segments.push(seg);
                }
            }
        } else if mode == "stroke" && stroke_width <= MAX_THICKNESS {
            for r in pending_rects.drain(..) {
                let (x0, y0) = ctm_apply(ctm, r.x, r.y);
                let (x1, y1) = ctm_apply(ctm, r.x + r.w, r.y + r.h);
                let id = format!("p{}-s{}", page_number, segments.len());
                push_stroked_rect_edges(
                    segments,
                    &id,
                    x0.min(x1),
                    y0.min(y1),
                    (x1 - x0).abs(),
                    (y1 - y0).abs(),
                );
            }
            for l in pending_lines.drain(..) {
                let (lx1, ly1) = ctm_apply(ctm, l.x1, l.y1);
                let (lx2, ly2) = ctm_apply(ctm, l.x2, l.y2);
                let dx = (lx2 - lx1).abs();
                let dy = (ly2 - ly1).abs();
                if (dx >= MIN_LENGTH && dy < 1.0) || (dy >= MIN_LENGTH && dx < 1.0) {
                    segments.push(Segment {
                        id: format!("p{}-s{}", page_number, segments.len()),
                        x1: lx1,
                        y1: ly1,
                        x2: lx2,
                        y2: ly2,
                    });
                }
            }
        }
        // TS clears BOTH pending arrays at the end of every flushPath call,
        // including fill mode (lines discarded) and thick strokes (everything
        // discarded). Without this, unpainted paths leak into later flushes.
        pending_rects.clear();
        pending_lines.clear();
    }

    while idx < tokens.len() {
        let t = tokens[idx].as_str();

        match t {
            "q" => {
                state_stack.push((ctm, stroke_width));
            }
            "Q" => {
                if let Some((saved_ctm, saved_sw)) = state_stack.pop() {
                    ctm = saved_ctm;
                    stroke_width = saved_sw;
                }
            }
            "cm" if idx >= 6 => {
                let a = tokens[idx - 6].parse::<f64>().unwrap_or(0.0);
                let b = tokens[idx - 5].parse::<f64>().unwrap_or(0.0);
                let c = tokens[idx - 4].parse::<f64>().unwrap_or(0.0);
                let d = tokens[idx - 3].parse::<f64>().unwrap_or(0.0);
                let e = tokens[idx - 2].parse::<f64>().unwrap_or(0.0);
                let f = tokens[idx - 1].parse::<f64>().unwrap_or(0.0);
                ctm = ctm_concat(&ctm, &[a, b, c, d, e, f]);
            }
            "w" if idx >= 1 => {
                if let Ok(w) = tokens[idx - 1].parse::<f64>() {
                    stroke_width = w;
                }
            }
            "re" if idx >= 4 => {
                let x = tokens[idx - 4].parse::<f64>().unwrap_or(f64::NAN);
                let y = tokens[idx - 3].parse::<f64>().unwrap_or(f64::NAN);
                let w = tokens[idx - 2].parse::<f64>().unwrap_or(f64::NAN);
                let h = tokens[idx - 1].parse::<f64>().unwrap_or(f64::NAN);
                if (x + y + w + h).is_finite() {
                    pending_rects.push(PRect { x, y, w, h });
                }
            }
            "m" if idx >= 2 => {
                cur_x = tokens[idx - 2].parse::<f64>().unwrap_or(0.0);
                cur_y = tokens[idx - 1].parse::<f64>().unwrap_or(0.0);
                path_start_x = cur_x;
                path_start_y = cur_y;
            }
            "l" if idx >= 2 => {
                let x2 = tokens[idx - 2].parse::<f64>().unwrap_or(0.0);
                let y2 = tokens[idx - 1].parse::<f64>().unwrap_or(0.0);
                pending_lines.push(PLine {
                    x1: cur_x,
                    y1: cur_y,
                    x2,
                    y2,
                });
                cur_x = x2;
                cur_y = y2;
            }
            "h" => {
                if cur_x != path_start_x || cur_y != path_start_y {
                    pending_lines.push(PLine {
                        x1: cur_x,
                        y1: cur_y,
                        x2: path_start_x,
                        y2: path_start_y,
                    });
                }
                cur_x = path_start_x;
                cur_y = path_start_y;
            }
            "f" | "F" | "f*" => {
                flush_path(
                    "fill",
                    page_number,
                    &mut segments,
                    &mut pending_rects,
                    &mut pending_lines,
                    &ctm,
                    stroke_width,
                );
            }
            "S" => {
                flush_path(
                    "stroke",
                    page_number,
                    &mut segments,
                    &mut pending_rects,
                    &mut pending_lines,
                    &ctm,
                    stroke_width,
                );
            }
            "s" => {
                if cur_x != path_start_x || cur_y != path_start_y {
                    pending_lines.push(PLine {
                        x1: cur_x,
                        y1: cur_y,
                        x2: path_start_x,
                        y2: path_start_y,
                    });
                }
                flush_path(
                    "stroke",
                    page_number,
                    &mut segments,
                    &mut pending_rects,
                    &mut pending_lines,
                    &ctm,
                    stroke_width,
                );
            }
            "B" | "B*" | "b" | "b*" => {
                flush_path(
                    "fill",
                    page_number,
                    &mut segments,
                    &mut pending_rects,
                    &mut pending_lines,
                    &ctm,
                    stroke_width,
                );
                flush_path(
                    "stroke",
                    page_number,
                    &mut segments,
                    &mut pending_rects,
                    &mut pending_lines,
                    &ctm,
                    stroke_width,
                );
            }
            "n" => {
                pending_rects.clear();
                pending_lines.clear();
            }
            _ => {}
        }

        idx += 1;
    }

    segments
}

/// Fast tokenizer for PDF content streams.
fn tokenize_content_stream(raw: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut i: usize = 0;

    while i < len {
        let ch = bytes[i];

        // Skip whitespace
        if ch <= 32 {
            i += 1;
            continue;
        }

        // Skip comments
        if ch == b'%' {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Skip string literals (...)
        if ch == b'(' {
            let mut depth: i32 = 1;
            i += 1;
            while i < len && depth > 0 {
                let c = bytes[i];
                if c == b'\\' {
                    i += 1;
                } else if c == b'(' {
                    depth += 1;
                } else if c == b')' {
                    depth -= 1;
                }
                i += 1;
            }
            continue;
        }

        // Skip hex strings <...>
        if ch == b'<' && i + 1 < len && bytes[i + 1] != b'<' {
            i += 1;
            while i < len && bytes[i] != b'>' {
                i += 1;
            }
            if i < len {
                i += 1;
            }
            continue;
        }

        // Skip dict delimiters << >>
        if ch == b'<' && i + 1 < len && bytes[i + 1] == b'<' {
            i += 2;
            continue;
        }
        if ch == b'>' && i + 1 < len && bytes[i + 1] == b'>' {
            i += 2;
            continue;
        }

        // Regular token
        let start = i;
        while i < len {
            let c = bytes[i];
            if c <= 32 || c == b'(' || c == b')' || c == b'<' || c == b'>' || c == b'%' {
                break;
            }
            i += 1;
        }
        if i > start {
            tokens.push(String::from_utf8_lossy(&bytes[start..i]).to_string());
        }
    }

    tokens
}

// ---------------------------------------------------------------------------
// Image region detection
// ---------------------------------------------------------------------------

const MIN_IMAGE_AREA: f64 = 5000.0;
/// Public shim for the fast image-extraction path.
pub(crate) const MIN_IMAGE_AREA_PUB: f64 = MIN_IMAGE_AREA;

/// Shared bbox → image-region conversion (both extraction paths).
fn image_regions_from_bboxes(
    bboxes: &[(f32, f32, f32, f32)],
    page_number: u32,
    page_height: f64,
) -> Vec<ImageRegion> {
    let mut regions: Vec<ImageRegion> = Vec::new();

    for &(x0, y0, x1, y1) in bboxes {
        // TS reads image bboxes from the stext JSON, whose C serializer
        // "(int)"-truncates x0, y0, x1-x0, y1-y0 — replicate that precision.
        let x = (x0 as i32) as f64;
        let y = (y0 as i32) as f64;
        let w = ((x1 - x0) as i32) as f64;
        let h = ((y1 - y0) as i32) as f64;

        if w * h < MIN_IMAGE_AREA {
            continue;
        }

        let pdf_top_y = page_height - y;

        regions.push(ImageRegion {
            id: format!("p{}-img{}", page_number, regions.len()),
            page_number,
            bbox: Rect { x, y, w, h },
            top_y: pdf_top_y,
        });
    }

    regions
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Render an image region from a PDF page as a PNG buffer.
pub fn render_image_region(input: &[u8], region: &ImageRegion) -> Result<Vec<u8>> {
    let doc = Document::from_bytes(input, "application/pdf")
        .map_err(|e| anyhow!("Failed to open PDF: {}", e))?;
    let page = doc
        .load_page(region.page_number as i32 - 1)
        .map_err(|e| anyhow!("Failed to load page: {}", e))?;

    let pad = 10.0_f32;
    let bx = region.bbox.x as f32 - pad;
    let by = region.bbox.y as f32 - pad;
    let _bw = region.bbox.w as f32 + 2.0 * pad;
    let _bh = region.bbox.h as f32 + 2.0 * pad;
    let scale = 2.0_f32;

    let matrix = mupdf::Matrix::new(scale, 0.0, 0.0, scale, -bx * scale, -by * scale);

    let pixmap = page
        .to_pixmap(&matrix, &mupdf::Colorspace::device_rgb(), false, false)
        .map_err(|e| anyhow!("Failed to render page: {}", e))?;

    let mut png_buf = Vec::new();
    pixmap
        .write_to(&mut png_buf, mupdf::ImageFormat::PNG)
        .map_err(|e| anyhow!("Failed to encode PNG: {}", e))?;
    Ok(png_buf)
}

/// Extract text boxes and vector segments from all pages of a PDF buffer.
///
/// The own engine (fast_extract) handles text-based PDFs — including
/// empty-password AES-256 — at a fraction of MuPDF's cost; anything it
/// cannot model faithfully (other encryption, rotated pages, no
/// extractable text, non-Flate-family filters, parse errors) falls back
/// to MuPDF. MARKIT_PDF_ENGINE=mupdf forces the fallback.
pub fn extract_pages(input: &[u8]) -> Result<Vec<PageContent>> {
    if std::env::var("MARKIT_PDF_ENGINE").as_deref() != Ok("mupdf") {
        match super::fast_extract::extract_pages_fast(input) {
            Ok(pages) => return Ok(pages),
            Err(e) => {
                log_fallback(&e);
            }
        }
    }
    extract_pages_mupdf(input)
}

/// MuPDF-based extraction: the fallback engine, and the differential
/// oracle for validating the own engine (examples/oracle_diff).
pub fn extract_pages_mupdf(input: &[u8]) -> Result<Vec<PageContent>> {
    // Text extraction never consumes color-managed values, but MuPDF's
    // default context runs lcms ICC transforms during page processing
    // (~5% of small-document conversion). Off, once per thread.
    disable_icc_once();

    let doc = Document::from_bytes(input, "application/pdf")
        .map_err(|e| anyhow!("Failed to open PDF: {}", e))?;

    let page_count = doc
        .page_count()
        .map_err(|e| anyhow!("Failed to get page count: {}", e))?;

    // Content-stream access needs the pdf_document view of the SAME
    // document: a borrow-cast (pdf_document_from_fz_document), not the
    // second full open + xref parse this used to do.
    let pdf_doc =
        PdfDocument::try_from(doc).map_err(|e| anyhow!("Failed to open PDF document: {}", e))?;
    let doc = &pdf_doc;

    let mut pages: Vec<PageContent> = Vec::new();

    for i in 0..page_count {
        let page_number = (i + 1) as u32;
        let page = doc
            .load_page(i)
            .map_err(|e| anyhow!("Failed to load page {}: {}", page_number, e))?;

        let bounds = page
            .bounds()
            .map_err(|e| anyhow!("Failed to get page bounds: {}", e))?;
        let page_height = (bounds.y1 - bounds.y0) as f64;

        // Extract text boxes and image regions from one stext run
        let (text_boxes, images) = extract_page_content(&page, page_number, page_height);

        // Extract vector segments from raw content stream
        let segments = if std::env::var("MARKIT_NO_SEGMENTS").is_ok() {
            Vec::new()
        } else {
            extract_segments_from_pdf_page(&pdf_doc, i, page_number)
        };

        pages.push(PageContent {
            page_number,
            text_boxes,
            segments,
            images,
        });
    }

    Ok(pages)
}

/// Extract segments from the raw content stream of a PDF page.
fn extract_segments_from_pdf_page(
    pdf_doc: &PdfDocument,
    page_index: i32,
    page_number: u32,
) -> Vec<Segment> {
    let pdf_page = match pdf_doc.load_pdf_page(page_index) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    let contents = match pdf_page.contents() {
        Ok(Some(c)) => c,
        _ => return Vec::new(),
    };

    // Read the content stream bytes
    let raw_bytes = if contents.is_array().unwrap_or(false) {
        // Multiple content streams — concatenate
        let len = contents.len().unwrap_or(0);
        let mut all_bytes: Vec<u8> = Vec::new();
        for j in 0..len as i32 {
            if let Ok(Some(stream)) = contents.get_array(j) {
                if let Ok(bytes) = stream.read_stream() {
                    all_bytes.extend_from_slice(&bytes);
                }
            }
        }
        all_bytes
    } else {
        match contents.read_stream() {
            Ok(bytes) => bytes,
            Err(_) => return Vec::new(),
        }
    };

    segments_from_content_bytes(&raw_bytes, page_number)
}

/// Shared content-stream bytes → segments (both extraction paths).
fn segments_from_content_bytes(raw_bytes: &[u8], page_number: u32) -> Vec<Segment> {
    let raw = String::from_utf8_lossy(raw_bytes);
    extract_segments_from_content_stream(&raw, page_number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// One combined stext run must yield the same text boxes as the
    /// split text-only run (see extract_page_content).
    #[test]
    fn combined_flags_match_split_runs() {
        let mut checked = 0;
        for name in ["intel-743621-007.pdf", "intel-743835-004.pdf"] {
            if !has_fixture(name) {
                continue;
            }
            let input = std::fs::read(fixture_path(name)).unwrap();
            let (pages, diffs) = probe_flags_equivalence(&input).unwrap();
            assert_eq!(diffs, 0, "{name}: {diffs} of {pages} pages diverged");
            checked += 1;
        }
        if checked == 0 {
            eprintln!("Skipping: no PDF fixtures found");
        }
    }

    /// The native stext walk must be indistinguishable from the JSON
    /// round-trip it replaced (mupdf as_json serializer equivalence).
    #[test]
    fn native_walk_matches_json_path() {
        let mut checked = 0;
        for name in ["intel-743621-007.pdf", "intel-743835-004.pdf"] {
            if !has_fixture(name) {
                continue;
            }
            let input = std::fs::read(fixture_path(name)).unwrap();
            let doc = Document::from_bytes(&input, "application/pdf").unwrap();
            let n = doc.page_count().unwrap();
            for i in 0..n {
                let page = doc.load_page(i).unwrap();
                let bounds = page.bounds().unwrap();
                let h = (bounds.y1 - bounds.y0) as f64;
                let native = extract_text_boxes(&page, (i + 1) as u32, h).unwrap();
                let json = extract_text_boxes_via_json(&page, (i + 1) as u32, h).unwrap();
                assert_eq!(
                    format!("{native:?}"),
                    format!("{json:?}"),
                    "{name} page {} diverged",
                    i + 1
                );
            }
            checked += 1;
        }
        if checked == 0 {
            eprintln!("Skipping: no PDF fixtures found");
        }
    }

    const FIXTURE_DIR: &str = "../test/fixtures/pdfs";

    fn fixture_path(name: &str) -> String {
        format!("{}/{}", FIXTURE_DIR, name)
    }

    fn has_fixture(name: &str) -> bool {
        Path::new(&fixture_path(name)).exists()
    }

    // ---------------------------------------------------------------------------
    // extractPages: basic structure
    // ---------------------------------------------------------------------------

    #[test]
    fn returns_pages_with_text_boxes_and_segments() {
        if !has_fixture("intel-743621-007.pdf") {
            eprintln!("Skipping: fixture intel-743621-007.pdf not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743621-007.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();

        assert_eq!(pages.len(), 17);

        for page in &pages {
            assert!(page.page_number > 0);
        }
    }

    #[test]
    fn extracts_text_from_title_page() {
        if !has_fixture("intel-743621-007.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743621-007.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        let p1 = &pages[0];

        let all_text: String = p1
            .text_boxes
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            all_text.contains("700 Series"),
            "Title page should contain '700 Series'"
        );
        assert!(
            all_text.contains("Platform Controller Hub"),
            "Title page should contain 'Platform Controller Hub'"
        );
    }

    #[test]
    fn skips_blank_pages() {
        if !has_fixture("intel-743621-007.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743621-007.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        let p5 = &pages[4];
        assert_eq!(p5.text_boxes.len(), 0);
        assert_eq!(p5.segments.len(), 0);
    }

    // ---------------------------------------------------------------------------
    // extractPages: text box properties
    // ---------------------------------------------------------------------------

    #[test]
    fn text_boxes_have_valid_bounds() {
        if !has_fixture("intel-743621-007.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743621-007.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        let p6 = &pages[5];

        for tb in &p6.text_boxes {
            assert!(
                tb.bounds.left < tb.bounds.right,
                "left < right: {} < {}",
                tb.bounds.left,
                tb.bounds.right
            );
            assert!(
                tb.bounds.bottom < tb.bounds.top,
                "bottom < top: {} < {}",
                tb.bounds.bottom,
                tb.bounds.top
            );
            assert!(!tb.text.is_empty());
        }
    }

    #[test]
    fn detects_bold_text_via_font_name() {
        if !has_fixture("intel-743621-007.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743621-007.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        let p8 = &pages[7];
        let bold_boxes: Vec<_> = p8.text_boxes.iter().filter(|tb| tb.is_bold).collect();
        assert!(!bold_boxes.is_empty(), "Page 8 should have bold text boxes");
    }

    #[test]
    fn has_font_size_information() {
        if !has_fixture("intel-743621-007.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743621-007.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        let p6 = &pages[5];

        let sizes: std::collections::HashSet<u32> = p6
            .text_boxes
            .iter()
            .map(|tb| (tb.font_size * 100.0) as u32)
            .collect();
        assert!(
            sizes.len() >= 2,
            "Should have at least 2 font sizes, got {}",
            sizes.len()
        );
    }

    // ---------------------------------------------------------------------------
    // extractPages: vector segments
    // ---------------------------------------------------------------------------

    #[test]
    fn extracts_segments_from_pages_with_tables() {
        if !has_fixture("intel-743621-007.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743621-007.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        let p8 = &pages[7];
        assert!(
            !p8.segments.is_empty(),
            "Page 8 should have vector segments"
        );
    }

    #[test]
    fn segments_are_horizontal_or_vertical_lines() {
        if !has_fixture("intel-743621-007.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743621-007.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        let p8 = &pages[7];

        for seg in &p8.segments {
            let is_h = (seg.y1 - seg.y2).abs() < 1.0;
            let is_v = (seg.x1 - seg.x2).abs() < 1.0;
            assert!(is_h || is_v, "Segment {:?} is neither H nor V", seg);
        }
    }

    #[test]
    fn applies_ctm_transforms_to_segment_coordinates() {
        if !has_fixture("intel-743835-004.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743835-004.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        let p14 = &pages[13];

        let h_segs: Vec<_> = p14
            .segments
            .iter()
            .filter(|s| (s.y1 - s.y2).abs() < 1.0)
            .collect();
        assert!(
            !h_segs.is_empty(),
            "Page 14 should have horizontal segments"
        );

        let text_ys: Vec<f64> = p14.text_boxes.iter().map(|t| t.bounds.bottom).collect();
        let seg_ys: Vec<f64> = h_segs.iter().map(|s| s.y1).collect();

        if !text_ys.is_empty() && !seg_ys.is_empty() {
            let text_min = text_ys.iter().cloned().fold(f64::INFINITY, f64::min);
            let text_max = text_ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let seg_min = seg_ys.iter().cloned().fold(f64::INFINITY, f64::min);
            let seg_max = seg_ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

            assert!(seg_max > text_min);
            assert!(seg_min < text_max);
        }
    }

    // ---------------------------------------------------------------------------
    // extractPages: image regions
    // ---------------------------------------------------------------------------

    #[test]
    fn detects_diagram_images() {
        if !has_fixture("intel-743835-004.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743835-004.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        if pages.len() > 194 {
            let p195 = &pages[194];
            assert!(!p195.images.is_empty(), "Page 195 should have images");
            assert!(p195.images[0].bbox.w > 100.0);
            assert!(p195.images[0].bbox.h > 50.0);
        }
    }

    #[test]
    fn does_not_detect_images_on_text_only_pages() {
        if !has_fixture("intel-743621-007.pdf") {
            eprintln!("Skipping: fixture not found");
            return;
        }

        let buf = std::fs::read(fixture_path("intel-743621-007.pdf")).unwrap();
        let pages = extract_pages(&buf).unwrap();
        let p6 = &pages[5];
        assert_eq!(p6.images.len(), 0, "Page 6 should have no images");
    }

    // ---------------------------------------------------------------------------
    // Unit tests that always run (no fixtures needed)
    // ---------------------------------------------------------------------------

    #[test]
    fn tokenize_content_stream_basic() {
        let tokens = tokenize_content_stream("10 20 30 40 re f");
        assert_eq!(tokens, vec!["10", "20", "30", "40", "re", "f"]);
    }

    #[test]
    fn tokenize_skips_comments() {
        let tokens = tokenize_content_stream("10 20 % comment\n30 40 re f");
        assert_eq!(tokens, vec!["10", "20", "30", "40", "re", "f"]);
    }

    #[test]
    fn tokenize_skips_string_literals() {
        let tokens = tokenize_content_stream("(hello) 10 20 m");
        assert_eq!(tokens, vec!["10", "20", "m"]);
    }

    #[test]
    fn tokenize_skips_hex_strings() {
        let tokens = tokenize_content_stream("<AABB> 10 20 m");
        assert_eq!(tokens, vec!["10", "20", "m"]);
    }

    #[test]
    fn thin_rect_horizontal() {
        let seg = thin_rect_to_segment("test".into(), 10.0, 20.0, 100.0, 0.5);
        assert!(seg.is_some());
        let s = seg.unwrap();
        assert!((s.y1 - s.y2).abs() < 0.001);
    }

    #[test]
    fn thin_rect_vertical() {
        let seg = thin_rect_to_segment("test".into(), 10.0, 20.0, 0.5, 100.0);
        assert!(seg.is_some());
        let s = seg.unwrap();
        assert!((s.x1 - s.x2).abs() < 0.001);
    }

    #[test]
    fn thin_rect_rejects_square() {
        let seg = thin_rect_to_segment("test".into(), 10.0, 20.0, 50.0, 50.0);
        assert!(seg.is_none());
    }

    #[test]
    fn merge_adjacent_items() {
        let raws = vec![
            RawTextItem {
                text: "Hello".into(),
                x: 10.0,
                y: 100.0,
                width: 30.0,
                height: 12.0,
                font_size: 12.0,
                is_bold: false,
            },
            RawTextItem {
                text: "World".into(),
                x: 45.0,
                y: 100.0,
                width: 30.0,
                height: 12.0,
                font_size: 12.0,
                is_bold: false,
            },
        ];
        let merged = merge_into_words(&raws);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Hello World");
    }

    #[test]
    fn merge_keeps_separate_lines() {
        let raws = vec![
            RawTextItem {
                text: "Line1".into(),
                x: 10.0,
                y: 100.0,
                width: 30.0,
                height: 12.0,
                font_size: 12.0,
                is_bold: false,
            },
            RawTextItem {
                text: "Line2".into(),
                x: 10.0,
                y: 80.0,
                width: 30.0,
                height: 12.0,
                font_size: 12.0,
                is_bold: false,
            },
        ];
        let merged = merge_into_words(&raws);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn ctm_identity_preserves_coords() {
        let (x, y) = ctm_apply(&CTM_IDENTITY, 10.0, 20.0);
        assert!((x - 10.0).abs() < 0.001);
        assert!((y - 20.0).abs() < 0.001);
    }

    #[test]
    fn ctm_translation_works() {
        let ctm: CTM = [1.0, 0.0, 0.0, 1.0, 100.0, 200.0];
        let (x, y) = ctm_apply(&ctm, 10.0, 20.0);
        assert!((x - 110.0).abs() < 0.001);
        assert!((y - 220.0).abs() < 0.001);
    }

    #[test]
    fn ctm_scaling_works() {
        let ctm: CTM = [2.0, 0.0, 0.0, 3.0, 0.0, 0.0];
        let (x, y) = ctm_apply(&ctm, 10.0, 20.0);
        assert!((x - 20.0).abs() < 0.001);
        assert!((y - 60.0).abs() < 0.001);
    }

    #[test]
    fn ctm_concat_identity_is_identity() {
        let result = ctm_concat(&CTM_IDENTITY, &CTM_IDENTITY);
        assert_eq!(result, CTM_IDENTITY);
    }

    #[test]
    fn extract_segments_basic_fill() {
        let stream = "10 20 100 0.5 re f";
        let segments = extract_segments_from_content_stream(stream, 1);
        assert!(
            !segments.is_empty(),
            "Should extract a horizontal line from thin rect"
        );
    }

    #[test]
    fn extract_segments_stroked_rect() {
        let stream = "10 20 100 50 re S";
        let segments = extract_segments_from_content_stream(stream, 1);
        assert_eq!(
            segments.len(),
            4,
            "Stroked rect should produce 4 edge segments"
        );
    }

    #[test]
    fn extract_segments_line_operators() {
        let stream = "1 w 10 100 m 200 100 l S";
        let segments = extract_segments_from_content_stream(stream, 1);
        assert!(
            !segments.is_empty(),
            "Should extract line from m/l/S operators"
        );
    }

    #[test]
    fn extract_segments_n_discards_path() {
        let stream = "10 20 100 0.5 re n";
        let segments = extract_segments_from_content_stream(stream, 1);
        assert!(
            segments.is_empty(),
            "n operator should discard pending path"
        );
    }

    #[test]
    fn extract_segments_with_ctm_transform() {
        let stream = "q 1 0 0 1 50 50 cm 10 20 100 0.5 re f Q";
        let segments = extract_segments_from_content_stream(stream, 1);
        assert!(!segments.is_empty());
        let seg = &segments[0];
        assert!(seg.x1 >= 50.0, "X should be translated: {}", seg.x1);
        assert!(seg.y1 >= 50.0, "Y should be translated: {}", seg.y1);
    }

    // Programmatic PDF test — always runs
    #[test]
    fn extract_pages_from_generated_pdf() {
        // Create a minimal PDF with mupdf
        use mupdf::pdf::PdfDocument;

        let mut doc = PdfDocument::new();
        let page = doc.new_page(mupdf::Size {
            width: 612.0,
            height: 792.0,
        });
        // We have a page — write the PDF out and read it back
        match page {
            Ok(_) => {
                let mut buf = Vec::new();
                match doc.write_to(&mut buf) {
                    Ok(_) => {
                        let pages = extract_pages(&buf).unwrap();
                        assert_eq!(pages.len(), 1);
                        assert_eq!(pages[0].page_number, 1);
                        // Empty page — no text, no segments
                        assert!(pages[0].text_boxes.is_empty());
                        assert!(pages[0].segments.is_empty());
                    }
                    Err(e) => {
                        eprintln!("Could not write PDF: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Could not create page: {}", e);
            }
        }
    }
}
