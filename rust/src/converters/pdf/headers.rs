//! Running header/footer detection and removal.
//!
//! Many PDFs have repeated text at the top or bottom of every page:
//! document titles, chapter names, page numbers, copyright notices.
//! These pollute the markdown output as false headings or noise.
//!
//! Algorithm:
//!   1. For each page, bucket text boxes by Y position (top/bottom zones)
//!   2. Collect the text content at each zone across all pages
//!   3. Text appearing on >20% of pages OR 8+ consecutive pages is a
//!      running header/footer
//!   4. Remove matching text boxes before further processing

use std::collections::{HashMap, HashSet};

use super::types::{PageContent, TextBox};

/// Minimum number of pages to enable header/footer detection.
const MIN_PAGES: usize = 5;

/// Fraction of page height treated as each running-margin zone.
const MARGIN_ZONE_RATIO: f64 = 0.12;

fn in_margin_zone(mid_y: f64, page_height: f64) -> bool {
    let margin = page_height * MARGIN_ZONE_RATIO;
    mid_y >= page_height - margin || mid_y <= margin
}

/// Minimum consecutive pages a text must appear on to be considered a
/// running header/footer.
const MIN_CONSECUTIVE_PAGES: usize = 8;

/// Detect and remove running headers and footers from all pages.
/// Mutates the pages slice in place, removing header/footer text boxes.
pub fn strip_headers_footers(pages: &mut [PageContent]) {
    if pages.len() < MIN_PAGES {
        return;
    }

    // Step 1: Build per-page zone text sets
    let mut page_zone_texts: Vec<HashSet<String>> = Vec::with_capacity(pages.len());

    for page in pages.iter() {
        let mut zone_texts: HashSet<String> = HashSet::new();
        for tb in &page.text_boxes {
            let mid_y = (tb.bounds.top + tb.bounds.bottom) / 2.0;
            if in_margin_zone(mid_y, page.page_height) {
                let key = normalize_whitespace(tb.text.trim());
                if !key.is_empty() {
                    zone_texts.insert(key);
                }
            }
        }
        page_zone_texts.push(zone_texts);
    }

    // Step 2: Collect all unique zone texts
    let mut all_texts: HashSet<String> = HashSet::new();
    for zts in &page_zone_texts {
        for t in zts {
            all_texts.insert(t.clone());
        }
    }

    // Step 3: Count global frequency AND longest consecutive run
    let mut global_count: HashMap<&str, usize> = HashMap::new();
    let mut max_consecutive: HashMap<&str, usize> = HashMap::new();

    for text in &all_texts {
        let mut total: usize = 0;
        let mut consecutive: usize = 0;
        let mut max_run: usize = 0;

        for zts in &page_zone_texts {
            if zts.contains(text.as_str()) {
                total += 1;
                consecutive += 1;
                if consecutive > max_run {
                    max_run = consecutive;
                }
            } else {
                consecutive = 0;
            }
        }

        global_count.insert(text.as_str(), total);
        max_consecutive.insert(text.as_str(), max_run);
    }

    // Step 4: Identify running headers/footers
    let global_threshold = 3_usize.max(pages.len() / 5); // floor(len * 0.2)
    let mut repeated_texts: HashSet<&str> = HashSet::new();

    for text in &all_texts {
        let gc = global_count.get(text.as_str()).copied().unwrap_or(0);
        let mc = max_consecutive.get(text.as_str()).copied().unwrap_or(0);

        if gc >= global_threshold {
            repeated_texts.insert(text.as_str());
            continue;
        }

        if mc >= MIN_CONSECUTIVE_PAGES {
            repeated_texts.insert(text.as_str());
        }
    }

    if repeated_texts.is_empty() {
        return;
    }

    // Step 5: Remove matching text boxes from each page
    for page in pages.iter_mut() {
        page.text_boxes.retain(|tb| {
            let mid_y = (tb.bounds.top + tb.bounds.bottom) / 2.0;
            if !in_margin_zone(mid_y, page.page_height) {
                return true;
            }

            let normalized = normalize_whitespace(tb.text.trim());
            !repeated_texts.contains(normalized.as_str())
        });
    }
}

/// Fraction of page height treated as the top/bottom band for the
/// single-page chrome detector. Wider than the repetition band: chrome on
/// a first page can sit a little deeper (e.g. a citation banner above a
/// paper title).
const SP_BAND_RATIO: f64 = 0.15;

/// Minimum gap, in multiples of the page's body font size, between a
/// chrome candidate and the nearest body-side line. Real chrome is
/// visually separated from content.
const SP_ISOLATION_GAP_RATIO: f64 = 1.0;

/// Chrome candidates longer than this are body prose, never chrome.
const SP_MAX_CHARS: usize = 200;

/// Maximum lines in a strippable chrome group. Running chrome is 1–4
/// short lines; a larger block is a title/abstract and must survive
/// even when a stray page number rides along.
const SP_MAX_GROUP_LINES: usize = 4;

/// Does the text match an unambiguous running-chrome signature?
/// URLs/DOIs, journal-banner phrases, copyright marks, `Page N`,
/// standalone page numbers, volume/issue markers, and journal citation
/// lines (year + page range, short). False positives on body prose are
/// the main risk — every branch here must be unambiguous.
pub(crate) fn matches_chrome_pattern(text: &str) -> bool {
    // Script sentinels (see shared.rs) ride along in extracted text
    // until render; they must not defeat the signatures.
    let owned: String;
    let text = if text.chars().any(|c| ('\u{E000}'..='\u{E003}').contains(&c)) {
        owned = text
            .chars()
            .filter(|c| !('\u{E000}'..='\u{E003}').contains(c))
            .collect();
        owned.as_str()
    } else {
        text
    };
    // Strip decorative rules and bullets before matching: banner lines
    // like "||WWW.EXAMPLE.COM" or "- 8 -" carry ornaments that would
    // defeat every prefix/whole-line signature.
    let t = text.trim_matches(|c: char| {
        c.is_whitespace() || "|\u{2022}\u{00b7}\u{25aa}\u{2013}\u{2014}-_=~*".contains(c)
    });
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();

    // URL / DOI prefixes — chrome lines are very often a citation URL.
    if lower.contains("http://")
        || lower.contains("https://")
        || lower.starts_with("www.")
        || lower.contains(" www.")
        || lower.contains("doi:")
        || lower.contains("doi.org/")
        || lower.contains("dx.doi.org")
    {
        return true;
    }

    // Common journal-paper banner phrases.
    if lower.contains("please cite this article")
        || lower.contains("contents lists available at")
        || lower.contains("available online at")
        || lower.contains("downloaded from")
    {
        return true;
    }

    // Copyright / trademark chrome.
    if t.contains('\u{a9}') || lower.contains("copyright ") || lower.contains("all rights reserved")
    {
        return true;
    }

    // "Page N" / "Page N of M".
    if let Some(rest) = lower.strip_prefix("page ") {
        if rest.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            return true;
        }
    }

    // Lone page number (≤4 digits) or roman-numeral folio.
    if !t.is_empty() && t.len() <= 4 && t.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    if (2..=7).contains(&lower.len()) && is_roman_numeral(&lower) {
        return true;
    }

    // Volume markers ("Vol. 24" / "Volume 81" / "v. 19, n. 1") with a
    // digit later.
    if has_vol_marker(&lower) || has_vn_marker(&lower) {
        return true;
    }

    // Author running heads ("Tao et al.", "Martinez et al. Respiratory
    // Research (2019)") — short lines only.
    if t.len() <= 80 && lower.contains("et al") {
        return true;
    }

    // Journal-cite style: a 4-digit year AND a numeric page range, short
    // enough to plausibly be chrome.
    if t.len() <= 120 && has_year(&lower) && has_digit_range(t) {
        return true;
    }

    // Volume:page citations ("The Astrophysical Journal, 811:51 (9pp)").
    if t.len() <= 120 && has_year(&lower) && has_colon_cite(t) {
        return true;
    }

    // "(1974). 7, 222" reference-style volume/page after a year.
    if t.len() <= 120 && has_paren_year_pages(t) {
        return true;
    }

    // A bare date line ("26 mai 1978", "March 8, 2003").
    if t.len() <= 30 && is_bare_date_line(t) {
        return true;
    }

    false
}

/// `NNN:NN` volume:page marker.
fn has_colon_cite(t: &str) -> bool {
    let b = t.as_bytes();
    for i in 1..b.len().saturating_sub(1) {
        if b[i] == b':' && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit() {
            return true;
        }
    }
    false
}

/// `(19xx)` or `(20xx)` followed by `N, N` volume/pages.
fn has_paren_year_pages(t: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\((?:19|20)\d\d\)\.? *\d+, *\d+").unwrap());
    RE.is_match(t)
}

/// `26 mai 1978` / `March 8, 2003` — a whole line that is only a date.
fn is_bare_date_line(t: &str) -> bool {
    static DMY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^\d{1,2}\.? +\p{L}+\.? +(?:19|20)\d\d\.?$").unwrap()
    });
    static MDY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^\p{L}+ +\d{1,2}, +(?:19|20)\d\d\.?$").unwrap()
    });
    DMY.is_match(t) || MDY.is_match(t)
}

/// `vol`/`volume` at a word start followed by `.`, ` ` or `,`, with any
/// digit later.
fn has_vol_marker(lower: &str) -> bool {
    let b = lower.as_bytes();
    for i in 0..b.len().saturating_sub(4) {
        let starts_word = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if !starts_word || b[i] != b'v' || b[i + 1] != b'o' || b[i + 2] != b'l' {
            continue;
        }
        // Optional "ume" suffix.
        let sep_at = if b[i + 3..].starts_with(b"ume") {
            i + 6
        } else {
            i + 3
        };
        if sep_at < b.len()
            && matches!(b[sep_at], b'.' | b' ' | b',')
            && b[sep_at + 1..].iter().any(u8::is_ascii_digit)
        {
            return true;
        }
    }
    false
}

/// Journal `v. N` / `n. N` abbreviations at a word start.
fn has_vn_marker(lower: &str) -> bool {
    let b = lower.as_bytes();
    for i in 0..b.len().saturating_sub(2) {
        let starts_word = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if starts_word && matches!(b[i], b'v' | b'n') && b[i + 1] == b'.' {
            let mut j = i + 2;
            if j < b.len() && b[j] == b' ' {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                return true;
            }
        }
    }
    false
}

/// Strict roman numeral (folio pages: `xvii`, `iv`, `xii`).
fn is_roman_numeral(lower: &str) -> bool {
    if lower.is_empty() || !lower.bytes().all(|b| b"mdclxvi".contains(&b)) {
        return false;
    }
    // Validate with the canonical grammar: thousands, hundreds, tens,
    // units, each segment optional.
    let mut s = lower;
    for _ in 0..3 {
        s = s.strip_prefix('m').unwrap_or(s);
    }
    for (nine, four, five, one) in [
        ("cm", "cd", 'd', 'c'),
        ("xc", "xl", 'l', 'x'),
        ("ix", "iv", 'v', 'i'),
    ] {
        if let Some(rest) = s.strip_prefix(nine).or_else(|| s.strip_prefix(four)) {
            s = rest;
            continue;
        }
        s = s.strip_prefix(five).unwrap_or(s);
        for _ in 0..3 {
            s = s.strip_prefix(one).unwrap_or(s);
        }
    }
    s.is_empty()
}

/// A 19xx/20xx group standing alone as a word.
fn has_year(lower: &str) -> bool {
    let b = lower.as_bytes();
    for i in 0..b.len().saturating_sub(3) {
        let starts_word = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if !starts_word {
            continue;
        }
        if ((b[i] == b'1' && b[i + 1] == b'9') || (b[i] == b'2' && b[i + 1] == b'0'))
            && b[i + 2].is_ascii_digit()
            && b[i + 3].is_ascii_digit()
        {
            let ends_word = i + 4 >= b.len() || !b[i + 4].is_ascii_alphanumeric();
            if ends_word {
                return true;
            }
        }
    }
    false
}

/// A digits–digits range that is not a `YYYY-MM`/`YYYY-MM-DD` calendar
/// date (a 4-digit 19xx/20xx group followed by a 1-2 digit group is a
/// date, not a page range).
fn has_digit_range(t: &str) -> bool {
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        let g1_len = j - i;
        let mut k = j;
        while k < chars.len() && chars[k] == ' ' {
            k += 1;
        }
        if k < chars.len() && matches!(chars[k], '-' | '\u{2013}' | '\u{2014}') {
            let mut m = k + 1;
            while m < chars.len() && chars[m] == ' ' {
                m += 1;
            }
            if m < chars.len() && chars[m].is_ascii_digit() {
                let mut n = m + 1;
                while n < chars.len() && chars[n].is_ascii_digit() {
                    n += 1;
                }
                let g2_len = n - m;
                let g1_is_year = g1_len == 4
                    && ((chars[i] == '1' && chars[i + 1] == '9')
                        || (chars[i] == '2' && chars[i + 1] == '0'));
                if !(g1_is_year && g2_len <= 2) {
                    return true;
                }
                i = n;
                continue;
            }
        }
        i = j;
    }
    false
}

/// Median font size of a page's text boxes — the body-size reference.
fn body_font_size(page: &PageContent) -> f64 {
    let mut sizes: Vec<f64> = page
        .text_boxes
        .iter()
        .map(|tb| tb.font_size)
        .filter(|s| *s > 0.0)
        .collect();
    if sizes.is_empty() {
        return 0.0;
    }
    sizes.sort_by(|a, b| a.total_cmp(b));
    sizes[sizes.len() / 2]
}

/// A standalone page-number line: bare digits or a roman folio.
fn is_lone_folio(text: &str) -> bool {
    let t = text.trim();
    if !t.is_empty() && t.len() <= 4 && t.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    let lower = t.to_lowercase();
    (1..=7).contains(&lower.len()) && is_roman_numeral(&lower)
}

/// Edge fraction of page height where extreme-most folios always strip.
const SP_EDGE_RATIO: f64 = 0.1;

/// Per-page chrome detection: position + isolation + pattern signature.
///
/// Complements `strip_headers_footers`: the repetition detector needs ≥5
/// pages, so single pages (and inconsistent per-page chrome) slip
/// through. A box is stripped when it sits fully inside the top or
/// bottom band, matches an unambiguous chrome pattern, is short, and is
/// separated from the nearest body-side non-chrome line by at least one
/// body-line height. Titles and headings never match the pattern gate,
/// so they survive regardless of position.
///
/// Coordinates are PDF user space: Y grows upward, `bounds.top` is the
/// numerically larger edge.
pub fn strip_single_page_chrome(pages: &mut [PageContent]) {
    for page in pages.iter_mut() {
        let h = page.page_height;
        if h <= 0.0 || page.text_boxes.is_empty() {
            continue;
        }
        let top_band_floor = h * (1.0 - SP_BAND_RATIO);
        let bottom_band_ceil = h * SP_BAND_RATIO;
        let body_size = body_font_size(page);
        let required_gap = SP_ISOLATION_GAP_RATIO * if body_size > 0.0 { body_size } else { 10.0 };
        let page_lines = count_lines(page.text_boxes.iter());

        // Band boxes chained into groups: consecutive lines (by Y) whose
        // inter-line gap is below the isolation gap belong together. A
        // running footer is often several lines (citation + page number);
        // one pattern hit licenses the whole group once the GROUP is
        // isolated from the body.
        let mut strip: HashSet<String> = HashSet::new();
        for band_top in [true, false] {
            let mut band_boxes: Vec<&TextBox> = page
                .text_boxes
                .iter()
                .filter(|tb| {
                    !tb.text.trim().is_empty()
                        && if band_top {
                            tb.bounds.bottom >= top_band_floor
                        } else {
                            tb.bounds.top <= bottom_band_ceil
                        }
                })
                .collect();
            band_boxes.sort_by(|a, b| b.bounds.top.total_cmp(&a.bounds.top));
            if band_boxes.is_empty() {
                continue;
            }

            // Chain into groups.
            let mut groups: Vec<Vec<&TextBox>> = Vec::new();
            let mut cur: Vec<&TextBox> = vec![band_boxes[0]];
            for tb in band_boxes.iter().skip(1) {
                let prev = cur.last().unwrap();
                let gap = prev.bounds.bottom - tb.bounds.top;
                if gap <= required_gap {
                    cur.push(tb);
                } else {
                    groups.push(std::mem::take(&mut cur));
                    cur.push(tb);
                }
            }
            groups.push(cur);

            for group in groups {
                // Both caps count visual LINES, not boxes: a wide
                // spread repeats its banner side by side, several
                // boxes on one line.
                let line_count = count_lines(group.iter().copied());
                // Lines carrying at least one chrome-pattern hit.
                let pattern_lines = {
                    let mut hits = 0usize;
                    let mut last_top = f64::INFINITY;
                    let mut line_hit = false;
                    for tb in &group {
                        if (last_top - tb.bounds.top).abs() > 3.0 {
                            hits += usize::from(line_hit);
                            line_hit = false;
                            last_top = tb.bounds.top;
                        }
                        line_hit |= matches_chrome_pattern(tb.text.trim());
                    }
                    hits + usize::from(line_hit)
                };
                // A short group strips on a single pattern hit; a
                // longer one (7-line journal banners: ISSN, volume,
                // copyright, URL…) needs a MAJORITY of chrome lines —
                // titles and abstracts never have that.
                if line_count > SP_MAX_GROUP_LINES && pattern_lines * 2 < line_count {
                    continue;
                }
                // A group that is most of the page is content, not chrome.
                if line_count * 2 > page_lines {
                    continue;
                }
                if group.iter().any(|tb| tb.text.trim().len() > SP_MAX_CHARS) {
                    continue;
                }
                if pattern_lines == 0 {
                    continue;
                }

                // Isolation: gap from the group's body-side edge to the
                // nearest line outside the group.
                let group_ids: HashSet<&str> = group.iter().map(|tb| tb.id.as_str()).collect();
                let edge = if band_top {
                    group
                        .iter()
                        .map(|tb| tb.bounds.bottom)
                        .fold(f64::INFINITY, f64::min)
                } else {
                    group
                        .iter()
                        .map(|tb| tb.bounds.top)
                        .fold(f64::NEG_INFINITY, f64::max)
                };
                let mut nearest = f64::INFINITY;
                for other in &page.text_boxes {
                    if group_ids.contains(other.id.as_str()) {
                        continue;
                    }
                    if band_top && other.bounds.top < edge {
                        nearest = nearest.min(edge - other.bounds.top);
                    } else if !band_top && other.bounds.bottom > edge {
                        nearest = nearest.min(other.bounds.bottom - edge);
                    }
                }
                if nearest < required_gap {
                    continue;
                }

                for tb in group {
                    strip.insert(tb.id.clone());
                }
            }
        }

        // A lone number that is the extreme-most line of the page, hard
        // against the margin, is a page number regardless of isolation —
        // folios sit directly beneath footnotes all the time. Runs after
        // group stripping so a folio can still anchor its chrome group.
        if page.text_boxes.len() > 2 {
            if let Some(bottom_most) = page
                .text_boxes
                .iter()
                .filter(|tb| !strip.contains(&tb.id))
                .min_by(|a, b| a.bounds.bottom.total_cmp(&b.bounds.bottom))
            {
                if is_lone_folio(&bottom_most.text)
                    && bottom_most.bounds.bottom <= h * SP_EDGE_RATIO
                {
                    strip.insert(bottom_most.id.clone());
                }
            }
            if let Some(top_most) = page
                .text_boxes
                .iter()
                .filter(|tb| !strip.contains(&tb.id))
                .max_by(|a, b| a.bounds.top.total_cmp(&b.bounds.top))
            {
                if is_lone_folio(&top_most.text) && top_most.bounds.top >= h * (1.0 - SP_EDGE_RATIO)
                {
                    strip.insert(top_most.id.clone());
                }
            }
        }

        if !strip.is_empty() {
            page.text_boxes.retain(|tb| !strip.contains(&tb.id));
        }
    }
}

/// Number of distinct visual lines (y-top clusters, 3pt tolerance)
/// among boxes sorted or not — sorts internally.
fn count_lines<'a>(boxes: impl Iterator<Item = &'a TextBox>) -> usize {
    let mut tops: Vec<f64> = boxes.map(|tb| tb.bounds.top).collect();
    tops.sort_by(|a, b| b.total_cmp(a));
    let mut lines = 0usize;
    let mut last_top = f64::INFINITY;
    for top in tops {
        if (last_top - top).abs() > 3.0 {
            lines += 1;
            last_top = top;
        }
    }
    lines
}

/// Collapse runs of whitespace to a single space.
fn normalize_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                result.push(' ');
                prev_space = true;
            }
        } else {
            result.push(ch);
            prev_space = false;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::types::{Bounds, TextBox};
    use super::*;

    fn make_text_box(text: &str, top: f64, bottom: f64, page_number: u32) -> TextBox {
        TextBox {
            id: format!("p{}-t0", page_number),
            text: text.to_string(),
            page_number,
            font_size: 10.0,
            is_bold: false,
            bounds: Bounds {
                left: 50.0,
                right: 200.0,
                top,
                bottom,
            },
        }
    }

    fn make_page(page_number: u32, texts: Vec<TextBox>) -> PageContent {
        PageContent {
            page_number,
            page_width: 612.0,
            page_height: 792.0,
            text_boxes: texts,
            segments: Vec::new(),
            images: Vec::new(),
        }
    }

    #[test]
    fn does_nothing_with_few_pages() {
        let mut pages = vec![
            make_page(1, vec![make_text_box("Header", 750.0, 740.0, 1)]),
            make_page(2, vec![make_text_box("Header", 750.0, 740.0, 2)]),
        ];
        strip_headers_footers(&mut pages);
        // Should not remove anything since < MIN_PAGES
        assert_eq!(pages[0].text_boxes.len(), 1);
    }

    #[test]
    fn strips_repeated_header() {
        let mut pages: Vec<PageContent> = (1..=10)
            .map(|i| {
                make_page(
                    i,
                    vec![
                        make_text_box("Document Title", 750.0, 740.0, i),
                        make_text_box("Body text here", 500.0, 490.0, i),
                    ],
                )
            })
            .collect();

        strip_headers_footers(&mut pages);

        // "Document Title" appears in top zone on all 10 pages — should be stripped
        for page in &pages {
            assert!(
                !page.text_boxes.iter().any(|tb| tb.text == "Document Title"),
                "Header should be stripped from page {}",
                page.page_number
            );
            // Body text should remain
            assert!(
                page.text_boxes.iter().any(|tb| tb.text == "Body text here"),
                "Body text should remain on page {}",
                page.page_number
            );
        }
    }

    #[test]
    fn strips_repeated_header_on_short_pages() {
        let mut pages: Vec<PageContent> = (1..=10)
            .map(|i| {
                let mut page = make_page(
                    i,
                    vec![
                        make_text_box("Short-page header", 370.0, 360.0, i),
                        make_text_box("Body", 220.0, 210.0, i),
                    ],
                );
                page.page_height = 400.0;
                page
            })
            .collect();
        strip_headers_footers(&mut pages);
        assert!(pages.iter().all(|page| page
            .text_boxes
            .iter()
            .all(|text| text.text != "Short-page header")));
    }

    #[test]
    fn strips_repeated_footer() {
        let mut pages: Vec<PageContent> = (1..=10)
            .map(|i| {
                make_page(
                    i,
                    vec![
                        make_text_box("Body text", 500.0, 490.0, i),
                        make_text_box("Copyright 2024", 40.0, 30.0, i),
                    ],
                )
            })
            .collect();

        strip_headers_footers(&mut pages);

        for page in &pages {
            assert!(
                !page.text_boxes.iter().any(|tb| tb.text == "Copyright 2024"),
                "Footer should be stripped from page {}",
                page.page_number
            );
        }
    }

    #[test]
    fn keeps_non_repeated_text() {
        let mut pages: Vec<PageContent> = (1..=10)
            .map(|i| {
                make_page(
                    i,
                    vec![
                        make_text_box(&format!("Unique title {}", i), 750.0, 740.0, i),
                        make_text_box("Body text", 500.0, 490.0, i),
                    ],
                )
            })
            .collect();

        strip_headers_footers(&mut pages);

        // Unique titles should remain (each appears only once)
        for page in &pages {
            assert!(
                page.text_boxes
                    .iter()
                    .any(|tb| tb.text.starts_with("Unique title")),
                "Unique title should remain on page {}",
                page.page_number
            );
        }
    }

    #[test]
    fn strips_consecutive_chapter_header() {
        let mut pages: Vec<PageContent> = (1..=20)
            .map(|i| {
                let mut texts = vec![make_text_box("Body", 500.0, 490.0, i)];
                // Chapter header appears on pages 5-15 (11 consecutive)
                if (5..=15).contains(&i) {
                    texts.push(make_text_box("Chapter 3", 750.0, 740.0, i));
                }
                make_page(i, texts)
            })
            .collect();

        strip_headers_footers(&mut pages);

        // "Chapter 3" appears on 11 consecutive pages (>= MIN_CONSECUTIVE_PAGES)
        for page in &pages {
            assert!(
                !page.text_boxes.iter().any(|tb| tb.text == "Chapter 3"),
                "Chapter header should be stripped from page {}",
                page.page_number
            );
        }
    }

    fn chrome_box(id: &str, text: &str, top: f64, bottom: f64) -> TextBox {
        TextBox {
            id: id.to_string(),
            text: text.to_string(),
            page_number: 1,
            font_size: 10.0,
            is_bold: false,
            bounds: Bounds {
                left: 50.0,
                right: 200.0,
                top,
                bottom,
            },
        }
    }

    fn single_page(text_boxes: Vec<TextBox>) -> Vec<PageContent> {
        vec![PageContent {
            page_number: 1,
            page_width: 612.0,
            page_height: 792.0,
            text_boxes,
            segments: Vec::new(),
            images: Vec::new(),
        }]
    }

    #[test]
    fn chrome_pattern_recognizes_common_signatures() {
        assert!(matches_chrome_pattern("http://example.com/foo"));
        assert!(matches_chrome_pattern("www.nature.com/scientificreports/"));
        assert!(matches_chrome_pattern(
            "Please cite this article in press as:"
        ));
        assert!(matches_chrome_pattern("Page 12 of 24"));
        assert!(matches_chrome_pattern("9"));
        assert!(matches_chrome_pattern("\u{a9} 2023 Acme Corp"));
        assert!(matches_chrome_pattern(
            "Cell Chemical Biology 24, 1\u{2013}9, November 16, 2017"
        ));
    }

    #[test]
    fn chrome_pattern_never_matches_body_prose_or_titles() {
        assert!(!matches_chrome_pattern(
            "The quick brown fox jumps over the lazy dog."
        ));
        assert!(!matches_chrome_pattern("Introduction"));
        // Year without a page range is a title, not chrome.
        assert!(!matches_chrome_pattern("Acme Annual Report 2023"));
        // YYYY-MM tracking dates are not page ranges.
        assert!(!matches_chrome_pattern("Company Tracking #: MS-2024-07"));
    }

    #[test]
    fn strips_isolated_top_band_url_on_single_page() {
        // 792pt page: top band is y >= 673.2.
        let mut pages = single_page(vec![
            chrome_box("chrome", "www.nature.com/scientificreports/", 780.0, 770.0),
            chrome_box("title", "Main Body Title", 600.0, 586.0),
            chrome_box("b1", "Body prose line one.", 570.0, 560.0),
        ]);
        strip_single_page_chrome(&mut pages);
        let ids: Vec<&str> = pages[0].text_boxes.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["title", "b1"]);
    }

    #[test]
    fn strips_bottom_journal_citation() {
        let mut pages = single_page(vec![
            chrome_box("b1", "Body line.", 500.0, 490.0),
            chrome_box("b2", "More body.", 480.0, 470.0),
            chrome_box(
                "cite",
                "Cell Chemical Biology 24, 1\u{2013}9, November 16, 2017",
                30.0,
                20.0,
            ),
        ]);
        strip_single_page_chrome(&mut pages);
        let ids: Vec<&str> = pages[0].text_boxes.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["b1", "b2"]);
    }

    #[test]
    fn preserves_band_title_without_chrome_pattern() {
        let mut pages = single_page(vec![
            chrome_box("title", "My Important Document", 770.0, 750.0),
            chrome_box("author", "Author Name", 730.0, 720.0),
            chrome_box("b1", "Body prose here.", 500.0, 490.0),
        ]);
        strip_single_page_chrome(&mut pages);
        assert_eq!(pages[0].text_boxes.len(), 3);
    }

    #[test]
    fn strips_multi_line_footer_group_with_one_pattern_hit() {
        // A citation line (patternless) directly above a page number:
        // the group strips as one unit.
        let mut pages = single_page(vec![
            chrome_box("b1", "Body prose one.", 500.0, 490.0),
            chrome_box("b2", "Body prose two.", 480.0, 470.0),
            chrome_box("b3", "Body prose three.", 460.0, 450.0),
            chrome_box("b4", "Body prose four.", 440.0, 430.0),
            chrome_box(
                "cite",
                "IRIARTE, Sara. El conjuro del matrerismo.",
                48.0,
                38.0,
            ),
            chrome_box("folio", "362", 30.0, 20.0),
        ]);
        strip_single_page_chrome(&mut pages);
        let ids: Vec<&str> = pages[0].text_boxes.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["b1", "b2", "b3", "b4"]);
    }

    #[test]
    fn chrome_pattern_sees_through_banner_ornaments() {
        assert!(matches_chrome_pattern("||WWW.CROUZET-MOTORS.COM"));
        assert!(matches_chrome_pattern("- 8 -"));
        assert!(matches_chrome_pattern("\u{2014} xii \u{2014}"));
        // Pure ornament is not chrome.
        assert!(!matches_chrome_pattern("||\u{2014}||"));
    }

    #[test]
    fn wide_spread_banner_on_one_line_strips_as_a_group() {
        // A landscape double-page spread repeats its banner side by
        // side: six boxes on ONE visual line must not blow the
        // group-line cap.
        fn banner(id: &str, text: &str, left: f64, right: f64) -> TextBox {
            TextBox {
                id: id.to_string(),
                text: text.to_string(),
                page_number: 1,
                font_size: 8.0,
                is_bold: false,
                bounds: Bounds {
                    left,
                    right,
                    top: 780.0,
                    bottom: 772.0,
                },
            }
        }
        let mut pages = single_page(vec![
            banner("h1", "||WWW.CROUZET-MOTORS.COM", 50.0, 170.0),
            banner("h2", "|| 8", 200.0, 212.0),
            banner("h3", "||DCmind MOTORS", 250.0, 440.0),
            banner("h4", "||WWW.CROUZET-MOTORS.COM", 640.0, 760.0),
            banner("h5", "|| 9", 790.0, 802.0),
            banner("h6", "||DCmind MOTORS", 840.0, 1030.0),
            chrome_box("b1", "Body prose line one.", 600.0, 590.0),
            chrome_box("b2", "Body prose line two.", 580.0, 570.0),
        ]);
        strip_single_page_chrome(&mut pages);
        let ids: Vec<&str> = pages[0].text_boxes.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["b1", "b2"]);
    }

    #[test]
    fn recognizes_et_al_vn_markers_and_roman_folios() {
        assert!(matches_chrome_pattern("Tao et al."));
        assert!(matches_chrome_pattern(
            "Scripta Uniandrade, v. 19, n. 1 (2021)"
        ));
        assert!(matches_chrome_pattern("xvii"));
        assert!(matches_chrome_pattern("Volume 81, Number 6, November 1975"));
        assert!(!matches_chrome_pattern("seven"));
        assert!(!matches_chrome_pattern("mixed"));
    }

    #[test]
    fn extreme_edge_folio_strips_despite_adjacent_footnotes() {
        // A page number directly beneath a footnote block: vertical
        // isolation fails, but the extreme-most lone number at the
        // margin is chrome regardless.
        let mut pages = single_page(vec![
            chrome_box("b1", "Body prose one.", 500.0, 490.0),
            chrome_box("b2", "Body prose two.", 480.0, 470.0),
            chrome_box("fn1", "8 Response given by the Minister.", 110.0, 100.0),
            chrome_box("fn2", "9 Ibid.", 96.0, 86.0),
            chrome_box("folio", "6", 85.0, 71.0),
        ]);
        strip_single_page_chrome(&mut pages);
        let ids: Vec<&str> = pages[0].text_boxes.iter().map(|t| t.id.as_str()).collect();
        assert!(!ids.contains(&"folio"), "{ids:?}");
        assert!(ids.contains(&"fn1"), "footnotes must survive: {ids:?}");
    }

    #[test]
    fn keeps_chrome_without_isolation_gap() {
        let mut pages = single_page(vec![
            chrome_box("url", "http://example.com/foo", 780.0, 770.0),
            // Next line 5pt below — well within one body-line height.
            chrome_box("b1", "Body line right after.", 765.0, 755.0),
            chrome_box("b2", "Continuing body.", 750.0, 740.0),
        ]);
        strip_single_page_chrome(&mut pages);
        assert_eq!(pages[0].text_boxes.len(), 3);
    }

    #[test]
    fn preserves_middle_zone_text() {
        let mut pages: Vec<PageContent> = (1..=10)
            .map(|i| {
                make_page(
                    i,
                    vec![
                        make_text_box("Same text everywhere", 400.0, 390.0, i), // middle zone
                    ],
                )
            })
            .collect();

        strip_headers_footers(&mut pages);

        // Text in the middle zone (not top/bottom) should never be stripped
        for page in &pages {
            assert_eq!(page.text_boxes.len(), 1);
        }
    }
}
