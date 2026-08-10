//! Show text divergence detail for one document (dev tooling).
use markit::converters::pdf::{extract, fast_extract};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let input = std::fs::read(&path).unwrap();
    let ours = fast_extract::extract_pages_fast(&input).unwrap();
    let oracle = extract::extract_pages_mupdf(&input).unwrap();
    for (o, m) in ours.iter().zip(oracle.iter()).take(2) {
        println!(
            "── page {} ours={} boxes, oracle={} boxes",
            o.page_number,
            o.text_boxes.len(),
            m.text_boxes.len()
        );
        let ours_text: std::collections::HashSet<&str> =
            o.text_boxes.iter().map(|t| t.text.as_str()).collect();
        for tb in m.text_boxes.iter().take(80) {
            if !ours_text.contains(tb.text.as_str()) {
                println!("  oracle-only: {:?}", &tb.text[..tb.text.len().min(80)]);
            }
        }
    }
}
