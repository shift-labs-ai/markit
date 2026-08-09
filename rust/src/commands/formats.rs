use crate::utils::output::{bold, dim, json_output, OutputOptions};

struct Format {
    name: &'static str,
    extensions: &'static [&'static str],
    builtin: bool,
}

const BUILTIN_FORMATS: &[Format] = &[
    Format { name: "PDF", extensions: &[".pdf"], builtin: true },
    Format { name: "Word", extensions: &[".docx"], builtin: true },
    Format { name: "PowerPoint", extensions: &[".pptx"], builtin: true },
    Format { name: "Excel", extensions: &[".xlsx"], builtin: true },
    Format { name: "HTML", extensions: &[".html", ".htm"], builtin: true },
    Format { name: "EPUB", extensions: &[".epub"], builtin: true },
    Format { name: "Jupyter", extensions: &[".ipynb"], builtin: true },
    Format { name: "RSS/Atom", extensions: &[".rss", ".atom", ".xml"], builtin: true },
    Format { name: "CSV", extensions: &[".csv", ".tsv"], builtin: true },
    Format { name: "JSON", extensions: &[".json"], builtin: true },
    Format { name: "YAML", extensions: &[".yaml", ".yml"], builtin: true },
    Format { name: "XML", extensions: &[".xml", ".svg"], builtin: true },
    Format { name: "Images", extensions: &[".jpg", ".png", ".gif", ".webp"], builtin: true },
    Format { name: "Audio", extensions: &[".mp3", ".wav", ".m4a", ".flac"], builtin: true },
    Format { name: "Pages", extensions: &[".pages"], builtin: true },
    Format { name: "Keynote", extensions: &[".key"], builtin: true },
    Format { name: "Numbers", extensions: &[".numbers"], builtin: true },
    Format { name: "GitHub", extensions: &["github.com/*", "gist.github.com/*"], builtin: true },
    Format { name: "ZIP", extensions: &[".zip"], builtin: true },
    Format { name: "Plain text", extensions: &[".txt", ".md", ".rst", ".log"], builtin: true },
    Format { name: "Code", extensions: &[".py", ".js", ".ts", ".go", ".rs", "..."], builtin: true },
    Format { name: "URLs", extensions: &["http://", "https://"], builtin: true },
    Format { name: "Wikipedia", extensions: &["*.wikipedia.org"], builtin: true },
];

pub fn formats(options: &OutputOptions) {
    if options.json {
        let list: Vec<serde_json::Value> = BUILTIN_FORMATS
            .iter()
            .map(|f| {
                serde_json::json!({
                    "name": f.name,
                    "extensions": f.extensions,
                    "builtin": f.builtin,
                })
            })
            .collect();
        json_output(&serde_json::json!({ "formats": list }));
        return;
    }

    // human output
    println!();
    println!("{}", bold("Supported formats"));
    println!();
    for fmt in BUILTIN_FORMATS {
        let exts = fmt.extensions.join(", ");
        println!("  {:<14} {}", fmt.name, dim(&exts));
    }
    // TODO(plugins): show plugin formats here
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_formats_has_expected_count() {
        // TS has 23 built-in formats
        assert_eq!(BUILTIN_FORMATS.len(), 23);
    }

    #[test]
    fn formats_list_starts_with_pdf() {
        assert_eq!(BUILTIN_FORMATS[0].name, "PDF");
    }

    #[test]
    fn formats_list_ends_with_wikipedia() {
        assert_eq!(BUILTIN_FORMATS[BUILTIN_FORMATS.len() - 1].name, "Wikipedia");
    }

    #[test]
    fn all_builtin_formats_are_builtin() {
        for f in BUILTIN_FORMATS {
            assert!(f.builtin);
        }
    }

    #[test]
    fn format_names_match_ts() {
        let names: Vec<&str> = BUILTIN_FORMATS.iter().map(|f| f.name).collect();
        assert_eq!(
            names,
            vec![
                "PDF", "Word", "PowerPoint", "Excel", "HTML", "EPUB", "Jupyter",
                "RSS/Atom", "CSV", "JSON", "YAML", "XML", "Images", "Audio",
                "Pages", "Keynote", "Numbers", "GitHub", "ZIP", "Plain text",
                "Code", "URLs", "Wikipedia",
            ]
        );
    }

    #[test]
    fn csv_has_tsv_extension() {
        let csv = BUILTIN_FORMATS.iter().find(|f| f.name == "CSV").unwrap();
        assert!(csv.extensions.contains(&".tsv"));
    }
}
