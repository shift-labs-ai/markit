//! Run borderless table detection on a page and print the decision.
use markit::converters::pdf::borderless::detect_borderless_tables;
use markit::converters::pdf::fast_extract::extract_pages_fast;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let data = std::fs::read(&path).unwrap();
    let pages = extract_pages_fast(&data).unwrap();
    for page in &pages {
        let (grids, consumed) = detect_borderless_tables(&page.text_boxes, page.page_number);
        println!(
            "page {}: {} grids, {} consumed of {} boxes",
            page.page_number,
            grids.len(),
            consumed.len(),
            page.text_boxes.len()
        );

        // Replay the row logic for diagnosis.
        let mut boxes: Vec<_> = page.text_boxes.iter().collect();
        boxes.sort_by(|a, b| {
            let ya = (a.bounds.top + a.bounds.bottom) / 2.0;
            let yb = (b.bounds.top + b.bounds.bottom) / 2.0;
            yb.total_cmp(&ya)
        });
        let mut rows: Vec<Vec<&markit::converters::pdf::types::TextBox>> = Vec::new();
        let mut mid_prev = f64::INFINITY;
        for tb in boxes {
            let mid = (tb.bounds.top + tb.bounds.bottom) / 2.0;
            if (mid_prev - mid).abs() <= 3.0 {
                rows.last_mut().unwrap().push(tb);
            } else {
                rows.push(vec![tb]);
                mid_prev = mid;
            }
        }
        for row in &rows {
            let mut sorted = row.clone();
            sorted.sort_by(|a, b| a.bounds.left.total_cmp(&b.bounds.left));
            let mut frags = 1usize;
            for pair in sorted.windows(2) {
                if pair[1].bounds.left - pair[0].bounds.right >= 15.0 {
                    frags += 1;
                }
            }
            let y = row[0].bounds.top;
            let first = &row[0].text;
            println!(
                "  row y={y:6.1} boxes={} frags={frags} {:?}",
                row.len(),
                &first[..first.len().min(30)]
            );
        }
        for g in &grids {
            println!("  {}x{} top_y={:.0}", g.rows, g.cols, g.top_y);
        }
    }
}
