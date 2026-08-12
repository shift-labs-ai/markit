//! Print resolved table grids and the vertical-line census for a page.
use markit::converters::pdf::fast_extract::extract_pages_fast;
use markit::converters::pdf::grid::resolve_table_grids;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let data = std::fs::read(&path).unwrap();
    let pages = extract_pages_fast(&data).unwrap();
    for page in &pages {
        let vertical: Vec<f64> = page
            .segments
            .iter()
            .filter(|s| (s.x1 - s.x2).abs() <= 0.8)
            .map(|s| s.x1)
            .collect();
        let mut xs = vertical.clone();
        xs.sort_by(|a, b| a.total_cmp(b));
        xs.dedup_by(|a, b| (*a - *b).abs() < 1.0);
        println!(
            "page {}: {} segments, {} vertical ({} unique x): {:?}",
            page.page_number,
            page.segments.len(),
            vertical.len(),
            xs.len(),
            &xs[..xs.len().min(20)]
        );
        let result = resolve_table_grids(page.page_number, &page.text_boxes, &page.segments);
        for g in &result.grids {
            println!(
                "  grid {}x{} borderless={} top_y={:.0}",
                g.rows, g.cols, g.is_borderless, g.top_y
            );
        }
    }
}
