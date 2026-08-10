//! opendataloader-bench runner: converts every PDF in a directory to
//! markdown predictions, recording warm per-document conversion time.
//! Usage: odl_bench <pdf-dir> <out-md-dir> <timings-json>
use markit::types::{Converter, StreamInfo};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (pdf_dir, out_dir, timing_path) = (&args[1], &args[2], &args[3]);
    std::fs::create_dir_all(out_dir).unwrap();

    let mut entries: Vec<_> = std::fs::read_dir(pdf_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    entries.sort();

    let conv = markit::converters::pdf::index::PdfConverter;
    let info = StreamInfo {
        extension: Some(".pdf".into()),
        ..Default::default()
    };

    let mut timings: Vec<(String, f64)> = Vec::new();
    let mut failures = 0usize;
    let total = Instant::now();
    for path in &entries {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path).unwrap();
        // warm-up + timed run, matching the bench's warm-conversion policy
        let _ = conv.convert(&bytes, &info);
        let t = Instant::now();
        match conv.convert(&bytes, &info) {
            Ok(r) => {
                let secs = t.elapsed().as_secs_f64();
                std::fs::write(format!("{out_dir}/{stem}.md"), &r.markdown).unwrap();
                timings.push((stem, secs));
            }
            Err(e) => {
                eprintln!("FAIL {stem}: {e}");
                failures += 1;
                // empty prediction so the evaluator scores the miss
                std::fs::write(format!("{out_dir}/{stem}.md"), "").unwrap();
                timings.push((stem, t.elapsed().as_secs_f64()));
            }
        }
    }
    let sum: f64 = timings.iter().map(|(_, s)| s).sum();
    let json = format!(
        "{{\"total_elapsed\": {sum}, \"wall\": {}, \"failures\": {failures}, \"docs\": {} }}",
        total.elapsed().as_secs_f64(),
        timings.len()
    );
    std::fs::write(timing_path, json).unwrap();
    eprintln!(
        "done: {} docs, {failures} failures, {sum:.2}s conversion",
        timings.len()
    );
}
