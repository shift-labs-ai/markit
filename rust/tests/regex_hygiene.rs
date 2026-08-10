//! Source-hygiene guard: every `Regex::new` in src/ must be compiled once
//! (inside a `LazyLock`/`OnceLock` static initializer), not per call.
//!
//! Per-call compilation is a real bug class we shipped once: the HTML→MD
//! engine recompiled 13 regexes on every conversion, making small-document
//! conversion ~19x slower than the TypeScript engine it replaced.
//!
//! Escape hatch for genuinely dynamic patterns: put a
//! `// regex-hygiene: allow — <reason>` comment within the five lines
//! preceding the call (and cache the compiled regex yourself, as
//! rss.rs::cached_tag_regex does).

use std::fs;
use std::path::{Path, PathBuf};

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn regex_new_is_never_compiled_per_call() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        src.display()
    );

    let mut violations: Vec<String> = Vec::new();

    for file in files {
        let content = fs::read_to_string(&file).expect("read source");
        let lines: Vec<&str> = content.lines().collect();

        // Skip everything from the test module down: test-only regexes are
        // compiled a handful of times and irrelevant to conversion speed.
        let test_start = lines
            .iter()
            .position(|l| l.contains("#[cfg(test)]"))
            .unwrap_or(lines.len());

        for (i, line) in lines[..test_start].iter().enumerate() {
            if !line.contains("Regex::new") {
                continue;
            }
            // Comments may mention Regex::new without compiling anything.
            if line.trim_start().starts_with("//") {
                continue;
            }
            // Compiled-once contexts: the initializer closure of a
            // LazyLock/OnceLock static within the preceding five lines
            // (rustfmt keeps these adjacent), or on the same line.
            let window_start = i.saturating_sub(5);
            let window = &lines[window_start..=i];
            let cached_once = window
                .iter()
                .any(|l| l.contains("LazyLock") || l.contains("OnceLock"));
            let allowed = window.iter().any(|l| l.contains("regex-hygiene: allow"));

            if !cached_once && !allowed {
                violations.push(format!(
                    "{}:{}: {}",
                    file.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap()
                        .display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Regex::new outside a LazyLock/OnceLock static (compiles per call!).\n\
         Cache it, or add `// regex-hygiene: allow — <reason>` with your own caching:\n{}",
        violations.join("\n")
    );
}
