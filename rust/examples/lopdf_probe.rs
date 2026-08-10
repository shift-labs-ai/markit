//! lopdf feasibility probe: parse every corpus PDF, decode all page
//! content streams, count text ops. Times the raw substrate our own
//! extractor would build on.
use lopdf::Document;
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

    // warm
    for p in &files {
        let b = std::fs::read(p).unwrap();
        let _ = Document::load_mem(&b);
    }

    let t = Instant::now();
    let (mut ok, mut fail, mut ops_total) = (0usize, 0usize, 0usize);
    for p in &files {
        let b = std::fs::read(p).unwrap();
        match Document::load_mem(&b) {
            Ok(doc) => {
                ok += 1;
                for (_num, page_id) in doc.get_pages() {
                    if let Ok(content) = doc.get_and_decode_page_content(page_id) {
                        ops_total += content.operations.len();
                    }
                }
            }
            Err(e) => {
                fail += 1;
                eprintln!("FAIL {}: {e}", p.display());
            }
        }
    }
    println!(
        "parsed+decoded {ok} ok / {fail} fail, {ops_total} ops, {:.3}s",
        t.elapsed().as_secs_f64()
    );
}
