//! In-process conversion benchmark: median of N runs per file.
//! Usage: bench_convert <file> [iters]
use markit::markit::Markit;
use std::time::Instant;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: bench_convert <file> [iters]");
    let iters: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let m = Markit::new();
    // warmup
    for _ in 0..iters.min(5) {
        let _ = m.convert_file(&path);
    }
    let mut times: Vec<f64> = Vec::with_capacity(iters);
    let mut len = 0usize;
    for _ in 0..iters {
        let t = Instant::now();
        let r = m.convert_file(&path).expect("convert failed");
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        len = r.markdown.len();
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{} median={:.3}ms min={:.3}ms len={}",
        path,
        times[iters / 2],
        times[0],
        len
    );
}
