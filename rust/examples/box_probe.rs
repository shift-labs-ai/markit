//! Geometry probe: fragment boxes and their gaps in a y-band.
use markit::converters::pdf::fast_extract::extract_pages_fast;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let y_lo: f64 = std::env::args().nth(2).unwrap().parse().unwrap();
    let y_hi: f64 = std::env::args().nth(3).unwrap().parse().unwrap();
    let data = std::fs::read(&path).unwrap();
    let pages = extract_pages_fast(&data).unwrap();
    for page in &pages {
        let mut boxes: Vec<_> = page
            .text_boxes
            .iter()
            .filter(|tb| tb.bounds.top <= y_hi && tb.bounds.bottom >= y_lo)
            .collect();
        boxes.sort_by(|a, b| {
            b.bounds
                .top
                .total_cmp(&a.bounds.top)
                .then(a.bounds.left.total_cmp(&b.bounds.left))
        });
        for tb in boxes.iter().take(40) {
            println!(
                "[{:6.1}..{:6.1}] y={:6.1} fs={:4.1} {:?}",
                tb.bounds.left,
                tb.bounds.right,
                tb.bounds.top,
                tb.font_size,
                &tb.text[..tb.text.len().min(60)]
            );
        }
    }
}
