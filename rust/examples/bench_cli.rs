//! Office-bench adapter: convert one document, markdown to stdout,
//! self-reported conversion time to stderr (anydoc bench-binary interface).
use markit::markit::Markit;
use std::io::Write;
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: bench_cli <file>");
    let m = Markit::new();
    let t = Instant::now();
    match m.convert_file(&path) {
        Ok(r) => {
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            std::io::stdout().write_all(r.markdown.as_bytes()).unwrap();
            eprintln!("Converted in {ms:.3}ms");
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
