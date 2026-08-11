#[test]
fn published_manifests_exclude_agpl_pdf_engines() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for path in [
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
        root.join("../package.json"),
        root.join("../bun.lock"),
    ] {
        let contents = std::fs::read_to_string(&path).unwrap();
        let lower = contents.to_ascii_lowercase();
        assert!(
            !lower.contains("mupdf"),
            "{} contains MuPDF",
            path.display()
        );
        assert!(!lower.contains("agpl"), "{} contains AGPL", path.display());
    }
}
