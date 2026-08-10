//! Phase timing across the ODL corpus: open, stext, walk+rest.
use markit::types::{Converter, StreamInfo};
use mupdf::pdf::PdfDocument;
use mupdf::{Document, TextPageFlags};
use std::time::Instant;

fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    files.sort();

    let conv = markit::converters::pdf::index::PdfConverter;
    let info = StreamInfo {
        extension: Some(".pdf".into()),
        ..Default::default()
    };
    // warm
    for p in &files {
        let b = std::fs::read(p).unwrap();
        let _ = conv.convert(&b, &info);
    }

    let (mut t_open, mut t_stext, mut t_full) = (0.0f64, 0.0f64, 0.0f64);
    for p in &files {
        let b = std::fs::read(p).unwrap();

        let t = Instant::now();
        let doc = Document::from_bytes(&b, "application/pdf").unwrap();
        let n = doc.page_count().unwrap();
        let pdoc = PdfDocument::try_from(doc).unwrap();
        t_open += t.elapsed().as_secs_f64();

        let t = Instant::now();
        for i in 0..n {
            let page = pdoc.load_page(i).unwrap();
            let flags = TextPageFlags::PRESERVE_WHITESPACE | TextPageFlags::PRESERVE_IMAGES;
            let tp = page.to_text_page(flags).unwrap();
            std::hint::black_box(&tp);
        }
        t_stext += t.elapsed().as_secs_f64();

        let t = Instant::now();
        let _ = conv.convert(&b, &info);
        t_full += t.elapsed().as_secs_f64();
    }
    println!(
        "open={t_open:.3}s stext(load+device)={t_stext:.3}s full_convert={t_full:.3}s rest≈{:.3}s",
        t_full - t_open - t_stext
    );
}
