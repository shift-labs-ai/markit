//! Regression corpus: PDFs that once failed conversion outright.
//! Sources: olmOCR-bench (ODC-BY-1.0, Allen Institute for AI).
//!
//! Every fixture must convert without error. Files with a recoverable
//! text layer must yield their text; image-only pages must yield image
//! placeholders, not an "unsupported encodings" failure.

use markit::converters::pdf::index::PdfConverter;
use markit::types::{Converter, StreamInfo};

fn convert(name: &str) -> String {
    let path = format!("testdata/pdf/regression/{name}");
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let info = StreamInfo {
        extension: Some(".pdf".into()),
        ..Default::default()
    };
    PdfConverter
        .convert(&data, &info)
        .unwrap_or_else(|e| panic!("{name}: conversion failed: {e}"))
        .markdown
}

#[test]
fn all_regression_fixtures_convert() {
    let mut count = 0;
    for entry in std::fs::read_dir("testdata/pdf/regression").unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|x| x != "pdf") {
            continue;
        }
        count += 1;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let md = convert(&name);
        assert!(!md.is_empty(), "{name}: empty output");
    }
    assert!(count >= 7, "regression corpus shrank: {count} PDFs");
}

/// dvips-style Type3 fonts with opaque /Differences names must recover
/// their ASCII-compatible text through the base-encoding fallback.
#[test]
fn type3_opaque_differences_recover_text() {
    let md = convert("09f90a8fad0997f7cf454cbcbe79cab3bc0f_page_1_pg1.pdf");
    assert!(
        md.contains("Open-Domain Textual Question Answering"),
        "title not recovered: {}",
        &md[..md.len().min(300)]
    );
}

/// A scanned page whose only text layer is whitespace is image-only,
/// not an encoding failure.
#[test]
fn whitespace_only_text_layer_is_image_only_page() {
    let md = convert("c655ed75d1f645f3ddc601f992a7d99c845a5e6f_page_8.pdf");
    assert!(md.contains("<!-- image:"), "no image placeholder: {md}");
}
