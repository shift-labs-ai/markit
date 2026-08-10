//! Profiling harness: times extract vs process phases.
//!
//! Findings (Apple M5, intel-743835-004.pdf, 224 pages): extraction is ~99%
//! of conversion time; header-stripping and rendering are ~1ms combined.
//! Parallel extraction across threads was tried and reverted: MuPDF
//! serializes allocation through a shared-context lock, so 2 threads gained
//! only 9% and >=3 threads were slower than sequential (10 threads: 2x
//! slower). Real PDF parallelism needs a lock-free parser (cf. anydoc's
//! lopdf+rayon), not more threads on MuPDF.
use markit::converters::pdf::extract::extract_pages;
use markit::converters::pdf::headers::strip_headers_footers;
use markit::types::{Converter, StreamInfo};

fn main() {
    let path = std::env::args().nth(1).expect("usage: pdf_profile <pdf>");
    let input = std::fs::read(&path).unwrap();

    let t0 = std::time::Instant::now();
    let mut pages = extract_pages(&input).unwrap();
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
