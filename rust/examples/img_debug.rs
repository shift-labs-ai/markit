//! Debug the fast image extractor on a document's first regions.
use markit::converters::pdf::image_extract::extract_image_region_fast;
use markit::types::{Converter, StreamInfo};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let input = std::fs::read(&path).unwrap();
    // Reproduce the region list via a full convert (no image_dir).
    let conv = markit::converters::pdf::index::PdfConverter;
    let info = StreamInfo {
        extension: Some(".pdf".into()),
        ..Default::default()
    };
    let _ = conv.convert(&input, &info);
    // Extract pages to get regions:
    let pages = markit::converters::pdf::fast_extract::extract_pages_fast(&input).unwrap();
    for page in pages.iter().take(3) {
        for img in &page.images {
            match extract_image_region_fast(&input, img) {
                Ok(x) => println!("{}: OK {} bytes .{}", img.id, x.bytes.len(), x.ext),
                Err(e) => println!("{}: ERR {e}", img.id),
            }
        }
    }
}
