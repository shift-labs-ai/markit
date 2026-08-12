//! Shared PDF extraction normalization used by the own engine.

use anyhow::Result;

use super::types::{Bounds, ImageRegion, Rect, Segment, TextBox};

const SAME_LINE_Y_TOLERANCE: f64 = 2.0;
/// Absolute ceiling on the horizontal merge gap (large display fonts).
const MAX_MERGE_GAP: f64 = 14.0;
/// Merge gap as a fraction of the font size. A word space is ~0.25em;
/// 0.6em accommodates wide justified spacing while staying well below
/// the narrowest two-column gutters (~10–12pt at 10pt body text),
/// which used to fuse adjacent columns into one line.
const MERGE_GAP_EM: f64 = 0.6;
/// Floor on the merge gap: dvips Type3 items carry no usable size or
/// height, and their word gaps run up to ~5.5pt.
const MIN_MERGE_GAP: f64 = 6.0;
const MIN_IMAGE_AREA: f64 = 5000.0;
const LINE_ASPECT_THRESHOLD: f64 = 6.0;
const MIN_LENGTH: f64 = 2.0;
const MAX_THICKNESS: f64 = 3.0;

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

pub(crate) struct RawTextItemPub {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub font_size: f64,
    pub is_bold: bool,
}

/// Private-use sentinels wrapping super/subscript runs, attached at
/// merge time (the only point where per-item baseline geometry still
/// exists) and consumed by the math emitter in render. Render strips
/// them from any non-math context.
pub(crate) const SUP_OPEN: char = '\u{E000}';
pub(crate) const SUP_CLOSE: char = '\u{E001}';
pub(crate) const SUB_OPEN: char = '\u{E002}';
pub(crate) const SUB_CLOSE: char = '\u{E003}';

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

/// Spacing accents that PDF text ops draw as separate glyphs overlaid
/// on their base letter, mapped to combining marks for composition.
fn spacing_accent_to_combining(c: char) -> Option<char> {
    Some(match c {
        '\u{B4}' | '\u{2CA}' => '\u{301}', // acute
        '`' | '\u{2CB}' => '\u{300}',      // grave
        '^' | '\u{2C6}' => '\u{302}',      // circumflex
        '~' | '\u{2DC}' => '\u{303}',      // tilde
        '\u{AF}' | '\u{2C9}' => '\u{304}', // macron
        '\u{2D8}' => '\u{306}',            // breve
        '\u{2D9}' => '\u{307}',            // dot above
        '\u{A8}' => '\u{308}',             // dieresis
        '\u{2DA}' => '\u{30A}',            // ring
        '\u{2DD}' => '\u{30B}',            // double acute
        '\u{2C7}' => '\u{30C}',            // caron
        '\u{B8}' => '\u{327}',             // cedilla
        '\u{2DB}' => '\u{328}',            // ogonek
        _ => return None,
    })
}

/// Compose base + combining into a single precomposed char, or None.
fn compose(base: char, combining: char) -> Option<char> {
    use unicode_normalization::UnicodeNormalization;
    let composed: String = [base, combining].iter().collect::<String>().nfc().collect();
    let mut chars = composed.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Space threshold for a line of per-glyph-positioned items whose
/// producer tracked the text tighter than the font's nominal advances
/// (Qt): every inter-glyph gap is then negative, but word gaps remain
/// a distinct upper mode. Split the sorted gap distribution at its
/// largest jump. None when the line is not per-glyph or not bimodal.
fn adaptive_space_threshold(line: &[RawTextItem]) -> Option<f64> {
    if line.len() < 12 {
        return None;
    }
    // The signature must be unambiguous: virtually every item a single
    // glyph (one Tj per glyph). Mixed lines (table cells, labels) keep
    // the absolute rule.
    let glyphy = line
        .iter()
        .filter(|item| item.text.trim().chars().count() <= 1)
        .count();
    if glyphy * 10 < line.len() * 9 {
        return None;
    }
    let mut gaps: Vec<f64> = line
        .windows(2)
        .map(|pair| pair[1].x - (pair[0].x + pair[0].width))
        .filter(|gap| gap.abs() < 6.0)
        .collect();
    if gaps.len() < 6 {
        return None;
    }
    gaps.sort_by(|a, b| a.total_cmp(b));
    // Only splits below the default threshold matter: gaps beyond
    // 1.0pt already read as spaces. The intra/word boundary of tracked
    // text lives in the negative range.
    let mut best_jump = 0.0;
    let mut split = None;
    for pair in gaps.windows(2) {
        let jump = pair[1] - pair[0];
        let mid = (pair[0] + pair[1]) / 2.0;
        if jump > best_jump && mid < 1.0 {
            best_jump = jump;
            split = Some(mid);
        }
    }
    // Both modes must exist and be clearly separated; and the default
    // absolute rule must have failed to see the upper mode (otherwise
    // leave the proven 1.0pt behavior alone).
    // Word gaps are the minority mode: most inter-glyph gaps sit
    // inside words. A "space" mode covering half the line is noise.
    let spaces_above = |t: f64| gaps.iter().filter(|g| **g > t).count();
    match split {
        Some(t) if best_jump >= 0.7 && t < 0.5 && spaces_above(t) * 3 <= gaps.len() => Some(t),
        _ => None,
    }
}

fn merge_into_words(raws: &[RawTextItem]) -> Vec<RawTextItem> {
    if raws.is_empty() {
        return Vec::new();
    }
    let cmp = |a: &RawTextItem, b: &RawTextItem| {
        let dy = b.y - a.y;
        if dy.abs() > SAME_LINE_Y_TOLERANCE && !script_same_line(a, b) {
            dy.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
        }
    };
    let mut sorted = raws.to_vec();
    super::js_stable_sort(&mut sorted, cmp);

    // Per-line space thresholds, computed on the sorted stream before
    // any merging: entry i covers sorted[i]'s line.
    let mut space_thresholds: Vec<f64> = vec![1.0; sorted.len()];
    {
        let mut line_start = 0usize;
        for i in 1..=sorted.len() {
            let line_ends = i == sorted.len() || {
                let a = &sorted[i - 1];
                let b = &sorted[i];
                (b.y - a.y).abs() > SAME_LINE_Y_TOLERANCE && !script_same_line(a, b)
            };
            if line_ends {
                if let Some(t) = adaptive_space_threshold(&sorted[line_start..i]) {
                    for slot in space_thresholds[line_start..i].iter_mut() {
                        *slot = t;
                    }
                }
                line_start = i;
            }
        }
    }

    let mut merged = Vec::new();
    let mut cur = sorted[0].clone();
    let mut cur_threshold = space_thresholds[0];
    for (next_index, next) in sorted.iter().enumerate().skip(1) {
        let same_y =
            (next.y - cur.y).abs() <= SAME_LINE_Y_TOLERANCE || script_same_line(&cur, next);
        // Type3/dvips fonts can carry a bogus nominal size; the glyph
        // box height is the reliable fallback reference.
        let ref_size = cur
            .font_size
            .max(next.font_size)
            .max(cur.height.max(next.height));
        let gap_cap = (MERGE_GAP_EM * ref_size).clamp(MIN_MERGE_GAP, MAX_MERGE_GAP);
        let close = next.x <= cur.x + cur.width + gap_cap;

        // Accent overlay: a spacing accent drawn over the previous
        // glyph (or the base drawn over a leading accent) composes into
        // the precomposed letter. Only when the glyphs overlap — real
        // text never overlaps, so literal `^`/`~` stay untouched.
        let overlaps = next.x < cur.x + cur.width - 1.0;
        if same_y && overlaps {
            let next_trim = next.text.trim();
            let mut next_chars = next_trim.chars();
            let (next_first, next_only) = (next_chars.next(), next_chars.next().is_none());
            // Base then accent.
            if let (Some(accent), true) = (next_first, next_only) {
                if let Some(combining) = spacing_accent_to_combining(accent) {
                    if let Some(base) = cur.text.chars().last() {
                        if let Some(composed) = compose(base, combining) {
                            cur.text.pop();
                            cur.text.push(composed);
                            cur.width = cur.width.max(next.x + next.width - cur.x);
                            continue;
                        }
                    }
                }
            }
            // Accent then base.
            if let Some(accent) = cur.text.chars().last() {
                if let Some(combining) = spacing_accent_to_combining(accent) {
                    if let Some(base) = next_first {
                        if let Some(composed) = compose(base, combining) {
                            cur.text.pop();
                            cur.text.push(composed);
                            let rest: String = next.text.trim_start().chars().skip(1).collect();
                            cur.text.push_str(&rest);
                            cur.width = cur.width.max(next.x + next.width - cur.x);
                            cur.height = cur.height.max(next.height);
                            cur.font_size = cur.font_size.max(next.font_size);
                            cur.is_bold |= next.is_bold;
                            continue;
                        }
                    }
                }
            }
        }

        if same_y && close {
            let sep = if next.x - (cur.x + cur.width) > cur_threshold {
                " "
            } else {
                ""
            };
            // A script item (smaller font, baseline shifted against the
            // line) records its role for the math emitter: raised →
            // superscript, lowered → subscript. Adjacent same-role
            // runs coalesce.
            let next_text: std::borrow::Cow<str> = if script_same_line(&cur, next)
                && next.font_size < cur.font_size
                && !next.text.trim().is_empty()
            {
                let mid = cur.y + cur.height * 0.5;
                let (open, close_c) = if next.y >= cur.y + cur.height * 0.28 {
                    (SUP_OPEN, SUP_CLOSE)
                } else if next.y + next.height <= mid + cur.height * 0.12 {
                    (SUB_OPEN, SUB_CLOSE)
                } else {
                    (' ', ' ')
                };
                if open != ' ' {
                    if sep.is_empty() && cur.text.ends_with(close_c) {
                        // Coalesce with the previous script run.
                        cur.text.pop();
                        std::borrow::Cow::Owned(format!("{}{}", next.text, close_c))
                    } else {
                        std::borrow::Cow::Owned(format!("{}{}{}", open, next.text, close_c))
                    }
                } else {
                    std::borrow::Cow::Borrowed(next.text.as_str())
                }
            } else {
                std::borrow::Cow::Borrowed(next.text.as_str())
            };
            cur.text = format!("{}{}{}", cur.text, sep, next_text);
            cur.width = next.x + next.width - cur.x;
            cur.height = cur.height.max(next.height);
            cur.font_size = cur.font_size.max(next.font_size);
            cur.is_bold |= next.is_bold;
        } else {
            merged.push(cur);
            cur = next.clone();
            cur_threshold = space_thresholds[next_index];
        }
    }
    merged.push(cur);
    merged
}

pub(crate) fn finish_text_boxes_pub(
    raws: Vec<RawTextItemPub>,
    page_number: u32,
) -> Result<Vec<TextBox>> {
    let raws = raws
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

/// Expand Unicode Latin ligature codepoints to their ASCII letters.
/// Fonts with a ToUnicode CMap frequently map the single “ﬁ” glyph to
/// U+FB01; search, matching, and downstream consumers want "fi".
fn expand_ligatures(text: &str) -> String {
    if !text.chars().any(|c| ('\u{FB00}'..='\u{FB06}').contains(&c)) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' => out.push_str("ft"),
            '\u{FB06}' => out.push_str("st"),
            _ => out.push(c),
        }
    }
    out
}

fn finish_text_boxes(raws: Vec<RawTextItem>, page_number: u32) -> Result<Vec<TextBox>> {
    Ok(merge_into_words(&raws)
        .into_iter()
        .enumerate()
        .map(|(i, item)| TextBox {
            id: format!("p{page_number}-t{i}"),
            text: expand_ligatures(
                match super::bidi::fix_rtl(&item.text) {
                    Some(text) => text.trim().to_string(),
                    None => item.text.trim().to_string(),
                }
                .as_str(),
            ),
            page_number,
            font_size: item.font_size,
            is_bold: item.is_bold,
            bounds: Bounds {
                left: item.x,
                right: item.x + item.width,
                bottom: item.y,
                top: item.y + item.height,
            },
        })
        .filter(|item| !item.text.is_empty())
        .collect())
}

pub(crate) fn image_bbox_is_large_pub((x0, y0, x1, y1): (f32, f32, f32, f32)) -> bool {
    let w = ((x1 - x0) as i32) as f64;
    let h = ((y1 - y0) as i32) as f64;
    w * h >= MIN_IMAGE_AREA
}

pub(crate) fn image_regions_from_bboxes_pub(
    bboxes: &[(f32, f32, f32, f32)],
    page_number: u32,
    page_height: f64,
) -> Vec<ImageRegion> {
    let mut regions = Vec::new();
    for &(x0, y0, x1, y1) in bboxes {
        if !image_bbox_is_large_pub((x0, y0, x1, y1)) {
            continue;
        }
        let x = (x0 as i32) as f64;
        let y = (y0 as i32) as f64;
        let w = ((x1 - x0) as i32) as f64;
        let h = ((y1 - y0) as i32) as f64;
        regions.push(ImageRegion {
            id: format!("p{page_number}-img{}", regions.len()),
            page_number,
            bbox: Rect { x, y, w, h },
            top_y: page_height - y,
        });
    }
    regions
}

pub(crate) fn thin_rect_to_segment_pub(x: f64, y: f64, w: f64, h: f64) -> Option<Segment> {
    let aw = w.abs();
    let ah = h.abs();
    if aw > ah * LINE_ASPECT_THRESHOLD && aw >= MIN_LENGTH && ah <= MAX_THICKNESS {
        let cy = y + ah / 2.0;
        return Some(Segment {
            id: String::new(),
            x1: x,
            y1: cy,
            x2: x + aw,
            y2: cy,
        });
    }
    if ah > aw * LINE_ASPECT_THRESHOLD && ah >= MIN_LENGTH && aw <= MAX_THICKNESS {
        let cx = x + aw / 2.0;
        return Some(Segment {
            id: String::new(),
            x1: cx,
            y1: y,
            x2: cx,
            y2: y + ah,
        });
    }
    None
}

pub(crate) fn push_stroked_rect_edges_pub(
    segments: &mut Vec<Segment>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) {
    let aw = w.abs();
    let ah = h.abs();
    if aw >= MIN_LENGTH {
        segments.push(Segment {
            id: String::new(),
            x1: x,
            y1: y,
            x2: x + aw,
            y2: y,
        });
        segments.push(Segment {
            id: String::new(),
            x1: x,
            y1: y + ah,
            x2: x + aw,
            y2: y + ah,
        });
    }
    if ah >= MIN_LENGTH {
        segments.push(Segment {
            id: String::new(),
            x1: x,
            y1: y,
            x2: x,
            y2: y + ah,
        });
        segments.push(Segment {
            id: String::new(),
            x1: x + aw,
            y1: y,
            x2: x + aw,
            y2: y + ah,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thin_rectangles_become_axis_aligned_segments() {
        assert!(thin_rect_to_segment_pub(0.0, 0.0, 100.0, 1.0).is_some());
        assert!(thin_rect_to_segment_pub(0.0, 0.0, 10.0, 10.0).is_none());
    }

    #[test]
    fn overlapping_spacing_accent_composes_with_base() {
        // "Guapore" + acute drawn over the final e → "Guaporé";
        // accent-first order composes too ("~" then "a" → "ã").
        let boxes = finish_text_boxes_pub(
            vec![
                RawTextItemPub {
                    text: "Guapore".into(),
                    x: 0.0,
                    y: 10.0,
                    width: 40.0,
                    height: 10.0,
                    font_size: 10.0,
                    is_bold: false,
                },
                RawTextItemPub {
                    text: "\u{B4}".into(),
                    x: 35.0,
                    y: 10.0,
                    width: 3.0,
                    height: 10.0,
                    font_size: 10.0,
                    is_bold: false,
                },
                RawTextItemPub {
                    text: "~".into(),
                    x: 50.0,
                    y: 10.0,
                    width: 4.0,
                    height: 10.0,
                    font_size: 10.0,
                    is_bold: false,
                },
                RawTextItemPub {
                    text: "ao".into(),
                    x: 50.0,
                    y: 10.0,
                    width: 10.0,
                    height: 10.0,
                    font_size: 10.0,
                    is_bold: false,
                },
            ],
            1,
        )
        .unwrap();
        assert_eq!(boxes.len(), 2, "{:?}", boxes[0].text);
        assert_eq!(boxes[0].text, "Guapor\u{e9}");
        assert_eq!(boxes[1].text, "\u{e3}o");
    }

    #[test]
    fn narrow_column_gutter_does_not_merge_lines() {
        // Two 10pt fragments separated by a 12pt gutter: distinct boxes.
        let boxes = finish_text_boxes_pub(
            vec![
                RawTextItemPub {
                    text: "left column text".into(),
                    x: 34.0,
                    y: 700.0,
                    width: 244.0,
                    height: 10.0,
                    font_size: 10.0,
                    is_bold: false,
                },
                RawTextItemPub {
                    text: "right column text".into(),
                    x: 290.0,
                    y: 700.0,
                    width: 240.0,
                    height: 10.0,
                    font_size: 10.0,
                    is_bold: false,
                },
            ],
            1,
        )
        .unwrap();
        assert_eq!(boxes.len(), 2, "{:?}", boxes[0].text);
    }

    #[test]
    fn ligature_codepoints_expand_to_ascii() {
        let boxes = finish_text_boxes_pub(
            vec![RawTextItemPub {
                text: "\u{FB01}gure \u{FB02}ow e\u{FB03}cient".into(),
                x: 0.0,
                y: 10.0,
                width: 60.0,
                height: 10.0,
                font_size: 10.0,
                is_bold: false,
            }],
            1,
        )
        .unwrap();
        assert_eq!(boxes[0].text, "figure flow efficient");
    }

    /// Qt PDFs draw one glyph per Tj and track the text tighter than
    /// the font's nominal advances: every inter-glyph gap is negative,
    /// but word gaps remain a distinct upper mode. The adaptive
    /// per-line threshold must recover the spaces.
    #[test]
    fn tracked_per_glyph_text_recovers_word_spaces() {
        let word_gap = -0.4;
        let letter_gap = -3.0;
        let mut items = Vec::new();
        let mut x = 0.0;
        for word in ["For", "comparability", "to", "our"] {
            for c in word.chars() {
                items.push(RawTextItemPub {
                    text: c.to_string(),
                    x,
                    y: 100.0,
                    width: 9.0,
                    height: 10.0,
                    font_size: 8.0,
                    is_bold: false,
                });
                x += 9.0 + letter_gap;
            }
            x += -letter_gap + word_gap; // undo last letter gap, add word gap
        }
        let boxes = finish_text_boxes_pub(items, 1).unwrap();
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].text, "For comparability to our");
    }

    /// Ordinary multi-char items keep the absolute 1pt rule — the
    /// adaptive threshold must not fire on mixed lines.
    #[test]
    fn mixed_line_keeps_absolute_space_rule() {
        let mut items = Vec::new();
        let mut x = 0.0;
        for (text, w) in [
            ("Q3", 12.0),
            ("0.74", 18.0),
            ("0.71", 18.0),
            ("13%", 16.0),
            ("4%", 12.0),
            ("x", 5.0),
            ("y", 5.0),
            ("z", 5.0),
            ("w", 5.0),
            ("v", 5.0),
            ("u", 5.0),
            ("t", 5.0),
        ] {
            items.push(RawTextItemPub {
                text: text.into(),
                x,
                y: 100.0,
                width: w,
                height: 10.0,
                font_size: 10.0,
                is_bold: false,
            });
            x += w + 0.5; // sub-threshold gaps: no spaces, one box
        }
        let boxes = finish_text_boxes_pub(items, 1).unwrap();
        assert_eq!(boxes.len(), 1);
        assert!(
            !boxes[0].text.contains(' '),
            "absolute rule changed: {:?}",
            boxes[0].text
        );
    }

    #[test]
    fn adjacent_items_merge_but_separate_lines_do_not() {
        let boxes = finish_text_boxes_pub(
            vec![
                RawTextItemPub {
                    text: "hello".into(),
                    x: 0.0,
                    y: 10.0,
                    width: 20.0,
                    height: 10.0,
                    font_size: 10.0,
                    is_bold: false,
                },
                RawTextItemPub {
                    text: "world".into(),
                    x: 24.0,
                    y: 10.0,
                    width: 20.0,
                    height: 10.0,
                    font_size: 10.0,
                    is_bold: false,
                },
                RawTextItemPub {
                    text: "next".into(),
                    x: 0.0,
                    y: 30.0,
                    width: 20.0,
                    height: 10.0,
                    font_size: 10.0,
                    is_bold: false,
                },
            ],
            1,
        )
        .unwrap();
        assert_eq!(boxes.len(), 2);
        assert!(boxes.iter().any(|item| item.text == "hello world"));
    }
}
