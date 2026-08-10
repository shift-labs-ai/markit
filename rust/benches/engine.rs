//! The performance gate. Run: cargo bench --bench engine
//!
//! Measures the PDF engine end to end on committed fixtures (median of
//! repeated runs, reported per stage), and — when MARKIT_BENCH_CORPUS
//! points at a directory of PDFs — the full-corpus conversion time that
//! backs the "fastest PDF engine" claim.
//!
//! Compare two checkouts: run on each, diff the printed table. A change
//! that moves a row past its committed ceiling does not land.

use std::time::Instant;

fn median_ms(mut f: impl FnMut(), warmup: usize, runs: usize) -> f64 {
    for _ in 0..warmup {
        f();
    }
    let mut times: Vec<f64> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times[times.len() / 2]
}

struct Row {
    name: &'static str,
    ms: f64,
    /// Regression ceiling (ms) on the reference machine (Apple M-class,
    /// release profile). Generous vs the observed median so CI noise
    /// does not trip it, tight enough to catch a real regression.
    ceiling: f64,
}

fn main() {
    let mut rows: Vec<Row> = Vec::new();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test/fixtures/pdfs");

    // Committed encrypted fixtures: parse + decrypt + extract.
    let enc = fixtures.join("encrypted");
    if enc.join("aes256.pdf").exists() {
        let plain = std::fs::read(enc.join("plain.pdf")).unwrap();
        let aes = std::fs::read(enc.join("aes256.pdf")).unwrap();
        let rc4 = std::fs::read(enc.join("rc4-128.pdf")).unwrap();
        rows.push(Row {
            name: "plain.pdf extract",
            ms: median_ms(
                || {
                    markit::converters::pdf::fast_extract::extract_pages_fast(&plain).unwrap();
                },
                20,
                200,
            ),
            ceiling: 0.5,
        });
        // AES-256 R6 pays ~4ms of mandated key schedule per open (two
        // Algorithm 2.B hardened hashes); the ceiling prices that in.
        rows.push(Row {
            name: "aes256.pdf extract",
            ms: median_ms(
                || {
                    markit::converters::pdf::fast_extract::extract_pages_fast(&aes).unwrap();
                },
                20,
                200,
            ),
            ceiling: 6.0,
        });
        rows.push(Row {
            name: "rc4-128.pdf extract",
            ms: median_ms(
                || {
                    markit::converters::pdf::fast_extract::extract_pages_fast(&rc4).unwrap();
                },
                20,
                200,
            ),
            ceiling: 0.6,
        });
    }

    // Private fixtures (present on dev machines, skipped elsewhere).
    for (file, ceiling) in [
        ("intel-743621-007.pdf", 30.0),
        ("intel-743835-004.pdf", 260.0),
    ] {
        let p = fixtures.join(file);
        if let Ok(bytes) = std::fs::read(&p) {
            let name: &'static str = Box::leak(file.to_string().into_boxed_str());
            rows.push(Row {
                name,
                ms: median_ms(
                    || {
                        markit::converters::pdf::fast_extract::extract_pages_fast(&bytes).unwrap();
                    },
                    3,
                    15,
                ),
                ceiling,
            });
        }
    }

    // Full corpus, when available.
    if let Ok(dir) = std::env::var("MARKIT_BENCH_CORPUS") {
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
            .collect();
        files.sort();
        let docs: Vec<Vec<u8>> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
        let info = markit::types::StreamInfo {
            extension: Some(".pdf".into()),
            ..Default::default()
        };
        let conv = markit::converters::pdf::index::PdfConverter;
        use markit::types::Converter;
        let ms = median_ms(
            || {
                for d in &docs {
                    let _ = conv.convert(d, &info);
                }
            },
            1,
            5,
        );
        let name: &'static str =
            Box::leak(format!("corpus ({} docs) convert", docs.len()).into_boxed_str());
        rows.push(Row {
            name,
            ms,
            ceiling: 90.0,
        });
    }

    let mut failed = false;
    println!("{:<32} {:>10} {:>10}", "benchmark", "median ms", "ceiling");
    for r in &rows {
        let flag = if r.ms > r.ceiling {
            failed = true;
            "  REGRESSION"
        } else {
            ""
        };
        println!("{:<32} {:>10.3} {:>10.1}{}", r.name, r.ms, r.ceiling, flag);
    }
    if rows.is_empty() {
        println!("no fixtures found — nothing measured");
    }
    if failed {
        std::process::exit(1);
    }
}
