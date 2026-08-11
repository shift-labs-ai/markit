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

use super::types::PageContent;

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
