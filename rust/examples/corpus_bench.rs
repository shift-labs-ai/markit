//! Ephemeral: convert every PDF under a dir in-process, report totals.
use markit::converters::pdf::index::PdfConverter;
use markit::types::{Converter, StreamInfo};
use std::time::Instant;

fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let mut files = Vec::new();
    for entry in walkdir(&dir) {
        files.push(entry);
    }
    files.sort();
    let info = StreamInfo {
        extension: Some(".pdf".into()),
        ..Default::default()
    };
    // warm: parse one file
    if let Some(f) = files.first() {
        let d = std::fs::read(f).unwrap();
        let _ = PdfConverter.convert(&d, &info);
    }
    let t = Instant::now();
    let (mut ok, mut fail) = (0u32, 0u32);
    for f in &files {
        let d = std::fs::read(f).unwrap();
        match PdfConverter.convert(&d, &info) {
            Ok(_) => ok += 1,
            Err(_) => fail += 1,
        }
    }
    let secs = t.elapsed().as_secs_f64();
    println!(
        "{} files, ok={ok} fail={fail}, {:.2}s, {:.1} docs/s",
        files.len(),
        secs,
        files.len() as f64 / secs
    );
}

fn walkdir(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_string()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p.to_string_lossy().into_owned());
            } else if p.extension().is_some_and(|x| x == "pdf") {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    out
}
