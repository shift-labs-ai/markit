//! Print the column-layout decision for one page.
use markit::converters::pdf::columns::detect_columns;
use markit::converters::pdf::fast_extract::extract_pages_fast;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let data = std::fs::read(&path).unwrap();
    let pages = extract_pages_fast(&data).unwrap();
    for page in &pages {
        let layout = detect_columns(&page.text_boxes, &page.segments);
        println!(
            "page {}: {} groups, boundaries {:?}",
            page.page_number, layout.column_count, layout.boundaries
        );
        for (i, (group, band)) in layout.columns.iter().zip(&layout.bands).enumerate() {
            let x0 = group
                .iter()
                .map(|b| b.bounds.left)
                .fold(f64::INFINITY, f64::min);
            let x1 = group
                .iter()
                .map(|b| b.bounds.right)
                .fold(f64::NEG_INFINITY, f64::max);
            let first = group.first().map(|b| b.text.as_str()).unwrap_or("");
            println!(
                "  g{i} band={band} n={} x=[{x0:.0}..{x1:.0}] {:?}",
                group.len(),
                &first[..first.len().min(40)]
            );
        }
    }
}
