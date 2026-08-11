//! Profiling harness: times extract vs process phases.
//!
//! Measures the own PDF engine's extraction and downstream rendering phases.
use markit::converters::pdf::fast_extract::extract_pages_fast;
use markit::converters::pdf::headers::strip_headers_footers;
use markit::types::{Converter, StreamInfo};

fn main() {
    let path = std::env::args().nth(1).expect("usage: pdf_profile <pdf>");
    let input = std::fs::read(&path).unwrap();

    let t0 = std::time::Instant::now();
    let mut pages = extract_pages_fast(&input).unwrap();
    let t_extract = t0.elapsed();

    let t1 = std::time::Instant::now();
    strip_headers_footers(&mut pages);
    let t_strip = t1.elapsed();

    let t2 = std::time::Instant::now();
    let c = markit::converters::pdf::index::PdfConverter;
    let info = StreamInfo {
        extension: Some(".pdf".into()),
        ..Default::default()
    };
    let r = c.convert(&input, &info).unwrap();
    let t_full = t2.elapsed();

    eprintln!(
        "pages={} extract={:?} strip={:?} full={:?} md_len={}",
        pages.len(),
        t_extract,
        t_strip,
        t_full,
        r.markdown.len()
    );
}
