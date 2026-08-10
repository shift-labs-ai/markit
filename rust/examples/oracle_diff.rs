//! MuPDF-as-oracle differential harness.
//!
//! Compares the own engine's extracted text against MuPDF's (the
//! battle-tested reference) document by document, scoring whitespace-
//! normalized trigram containment both ways. Low scores mark documents
//! where our extraction diverges — the quality worklist.
//!
//! Dev tooling only: MuPDF (AGPL) never ships in the published artifact.

use markit::converters::pdf::{extract, fast_extract};
use rustc_hash::FxHashSet;

fn page_text(pages: &[markit::converters::pdf::types::PageContent]) -> String {
    let mut out = String::new();
    for p in pages {
        for tb in &p.text_boxes {
            out.push_str(&tb.text);
            out.push(' ');
        }
    }
    out
}

fn trigrams(s: &str) -> FxHashSet<u64> {
    let norm: Vec<char> = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect();
    let mut set = FxHashSet::default();
    for w in norm.windows(3) {
        let mut h = 0xcbf29ce484222325u64;
        for &c in w {
            h = (h ^ c as u64).wrapping_mul(0x100000001b3);
        }
        set.insert(h);
    }
    set
}

fn containment(a: &FxHashSet<u64>, b: &FxHashSet<u64>) -> f64 {
    if a.is_empty() {
        return 1.0;
    }
    a.intersection(b).count() as f64 / a.len() as f64
}

fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    files.sort();

    let mut rows: Vec<(String, f64, f64)> = Vec::new();
    for p in &files {
        let input = std::fs::read(p).unwrap();
        let ours = match fast_extract::extract_pages_fast(&input) {
            Ok(pages) => page_text(&pages),
            Err(e) => {
                eprintln!("SKIP {} (fast path: {e})", p.display());
                continue;
            }
        };
        // Oracle: the MuPDF-based extraction (same downstream shape).
        let oracle = match extract::extract_pages_mupdf(&input) {
            Ok(pages) => page_text(&pages),
            Err(e) => {
                eprintln!("SKIP {} (oracle: {e})", p.display());
                continue;
            }
        };
        let (to, tm) = (trigrams(&ours), trigrams(&oracle));
        // oracle→ours: did we DROP content; ours→oracle: did we INVENT it.
        rows.push((
            p.file_name().unwrap().to_string_lossy().into_owned(),
            containment(&tm, &to),
            containment(&to, &tm),
        ));
    }

    rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let n = rows.len().max(1);
    let mean_recall: f64 = rows.iter().map(|r| r.1).sum::<f64>() / n as f64;
    let mean_precision: f64 = rows.iter().map(|r| r.2).sum::<f64>() / n as f64;
    println!(
        "docs={} oracle-recall={mean_recall:.4} oracle-precision={mean_precision:.4}",
        rows.len()
    );
    println!("worst 12 by recall (oracle content we drop):");
    for (name, r, p) in rows.iter().take(12) {
        println!("  {name}  recall={r:.3} precision={p:.3}");
    }
}
