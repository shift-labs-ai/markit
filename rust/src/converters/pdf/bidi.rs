//! Visual-to-logical reordering for RTL text.
//!
//! PDF content streams draw glyphs in visual order (left to right on
//! the page); Hebrew and Arabic text therefore arrives reversed, and
//! Arabic arrives as presentation forms (U+FB50..U+FDFF, U+FE70..
//! U+FEFF). Markdown wants logical order and base characters. The
//! heuristic: reverse the line, then restore embedded LTR islands
//! (Latin, digits) — the practical inverse of the display algorithm
//! for the overwhelmingly common single-level case.

use unicode_normalization::UnicodeNormalization;

fn is_rtl(c: char) -> bool {
    matches!(c,
        '\u{0590}'..='\u{05FF}'   // Hebrew
        | '\u{0600}'..='\u{06FF}' // Arabic
        | '\u{0700}'..='\u{074F}' // Syriac
        | '\u{0750}'..='\u{077F}' // Arabic Supplement
        | '\u{08A0}'..='\u{08FF}' // Arabic Extended-A
        | '\u{FB1D}'..='\u{FB4F}' // Hebrew presentation
        | '\u{FB50}'..='\u{FDFF}' // Arabic presentation A
        | '\u{FE70}'..='\u{FEFF}' // Arabic presentation B
    )
}

fn is_strong_ltr(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '\u{00C0}'..='\u{024F}')
}

fn is_presentation(c: char) -> bool {
    matches!(c, '\u{FB1D}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}')
}

/// Reorder a visually-ordered line into logical order when it contains
/// RTL script, normalizing presentation forms to base characters.
pub fn fix_rtl(text: &str) -> Option<String> {
    if !text.chars().any(is_rtl) {
        return None;
    }

    // Presentation forms → base letters (NFKC), only where present so
    // Latin ligatures and friends stay untouched.
    let mapped: Vec<char> = text
        .chars()
        .flat_map(|c| {
            if is_presentation(c) {
                c.nfkc().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect();

    // Reverse the whole line…
    let mut out: Vec<char> = mapped.into_iter().rev().collect();

    // …then restore LTR islands: maximal runs of strong-LTR characters
    // extended across interior neutrals (digits keep European order).
    let mut i = 0usize;
    while i < out.len() {
        if is_strong_ltr(out[i]) {
            let mut j = i + 1;
            let mut last_strong = i;
            while j < out.len() && !is_rtl(out[j]) {
                if is_strong_ltr(out[j]) {
                    last_strong = j;
                }
                j += 1;
            }
            out[i..=last_strong].reverse();
            i = j;
        } else {
            i += 1;
        }
    }

    Some(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ltr_untouched() {
        assert!(fix_rtl("plain English 123").is_none());
    }

    #[test]
    fn hebrew_reversed() {
        // "שלום" drawn visually becomes reversed in the extraction.
        let visual: String = "שלום".chars().rev().collect();
        assert_eq!(fix_rtl(&visual).unwrap(), "שלום");
    }

    #[test]
    fn ltr_island_survives() {
        // Visual: [reversed hebrew] ABC [reversed hebrew] — logical
        // order restores hebrew and keeps ABC forward.
        let heb1: String = "אבג".chars().rev().collect();
        let heb2: String = "דהו".chars().rev().collect();
        let visual = format!("{heb2} ABC {heb1}");
        assert_eq!(fix_rtl(&visual).unwrap(), "אבג ABC דהו");
    }

    #[test]
    fn arabic_presentation_normalized() {
        // U+FEED is the isolated form of WAW (U+0648).
        let fixed = fix_rtl("\u{FEED}").unwrap();
        assert_eq!(fixed, "\u{0648}");
    }
}
