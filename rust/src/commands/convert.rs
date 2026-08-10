use std::fs;
use std::io::{self, IsTerminal, Read, Write};

use crate::markit::Markit;
use crate::types::StreamInfo;
use crate::utils::output::{dim, error, output, success, write_stdout, OutputOptions};

const EXIT_ERROR: u8 = 1;
const EXIT_UNSUPPORTED: u8 = 3;

pub struct ConvertOptions {
    pub json: bool,
    pub quiet: bool,
    pub output_file: Option<String>,
    pub image_dir: Option<String>,
}

pub fn convert(source: &str, opts: &ConvertOptions) -> u8 {
    let markit = Markit::new();

    // Auto-create a temp dir for images if not explicitly provided
    let image_dir = opts.image_dir.clone().unwrap_or_else(|| {
        std::env::temp_dir()
            .join(format!("markit-images-{}", std::process::id()))
            .to_string_lossy()
            .to_string()
    });

    let is_stdin = source == "-";
    let is_url =
        source.starts_with("http:") || source.starts_with("https:") || source.starts_with("file:");

    let result = if is_stdin {
        // Check if stdin is a TTY (no piped input)
        if io::stdin().is_terminal() {
            error("No input on stdin. Pipe a file: cat report.pdf | markit -");
            return EXIT_ERROR;
        }
        let mut buffer = Vec::new();
        if let Err(e) = io::stdin().read_to_end(&mut buffer) {
            error(&e.to_string());
            return EXIT_ERROR;
        }
        let info = StreamInfo {
            image_dir: Some(image_dir),
            ..Default::default()
        };
        markit.convert(&buffer, &info)
    } else if is_url {
        // Progress hint for URL fetches (stderr so it doesn't pollute piped output)
        if !opts.json && !opts.quiet {
            let _ = io::stderr().write_all(format!("ℹ Fetching {}...\n", source).as_bytes());
            let _ = io::stderr().flush();
        }
        markit.convert_url(source)
    } else {
        // Check file exists first for better error message
        if !std::path::Path::new(source).exists() {
            let out_opts = OutputOptions {
                json: opts.json,
                quiet: opts.quiet,
            };
            output(
                &out_opts,
                || serde_json::json!({ "success": false, "error": format!("File not found: {}", source) }),
                None::<fn()>,
                || error(&format!("File not found: {}", source)),
            );
            return EXIT_ERROR;
        }
        markit.convert_file_with(
            source,
            StreamInfo {
                image_dir: Some(image_dir.clone()),
                ..Default::default()
            },
        )
    };

    let out_opts = OutputOptions {
        json: opts.json,
        quiet: opts.quiet,
    };

    match result {
        Ok(res) => {
            let label = if is_stdin { "stdin" } else { source };

            if let Some(ref out_path) = opts.output_file {
                if let Err(e) = fs::write(out_path, &res.markdown) {
                    error(&format!("Error writing {}: {}", out_path, e));
                    return EXIT_ERROR;
                }
                output(
                    &out_opts,
                    || {
                        // TS JSON.stringify drops undefined title — omit None.
                        // Key order matches TS: success, source, output, title, length.
                        let mut map = serde_json::Map::new();
                        map.insert("success".into(), serde_json::json!(true));
                        map.insert("source".into(), serde_json::json!(label));
                        map.insert("output".into(), serde_json::json!(out_path));
                        if let Some(ref title) = res.title {
                            map.insert("title".into(), serde_json::json!(title));
                        }
                        // TS .length is UTF-16 code units, not bytes.
                        map.insert(
                            "length".into(),
                            serde_json::json!(res.markdown.encode_utf16().count()),
                        );
                        serde_json::Value::Object(map)
                    },
                    None::<fn()>,
                    || {
                        success(&format!("Converted → {}", out_path));
                        if let Some(ref title) = res.title {
                            println!("{}", dim(&format!("  title: {}", title)));
                        }
                        println!(
                            "{}",
                            dim(&format!("  {} chars", res.markdown.encode_utf16().count()))
                        );
                    },
                );
            } else {
                output(
                    &out_opts,
                    || {
                        // TS JSON.stringify drops undefined title — omit None.
                        // Key order matches TS: success, source, title, markdown.
                        let mut map = serde_json::Map::new();
                        map.insert("success".into(), serde_json::json!(true));
                        map.insert("source".into(), serde_json::json!(label));
                        if let Some(ref title) = res.title {
                            map.insert("title".into(), serde_json::json!(title));
                        }
                        map.insert("markdown".into(), serde_json::json!(res.markdown));
                        serde_json::Value::Object(map)
                    },
                    Some(|| write_stdout(&res.markdown)),
                    || write_stdout(&res.markdown),
                );
            }
            0
        }
        Err(err) => {
            let msg = err.to_string();

            if msg.contains("Unsupported format") {
                output(
                    &out_opts,
                    || serde_json::json!({ "success": false, "error": msg }),
                    None::<fn()>,
                    || {
                        error(&msg);
                        println!(
                            "{}",
                            dim("  Run 'markit formats' to see supported formats.")
                        );
                    },
                );
                return EXIT_UNSUPPORTED;
            }

            // TS: msg.includes("ENOENT") || msg.includes("no such file").
            // Rust io errors say "No such file or directory"; keep the match
            // that narrow so converter errors are not misreported.
            if msg.contains("ENOENT") || msg.contains("No such file") {
                output(
                    &out_opts,
                    || serde_json::json!({ "success": false, "error": format!("File not found: {}", source) }),
                    None::<fn()>,
                    || error(&format!("File not found: {}", source)),
                );
                return EXIT_ERROR;
            }

            output(
                &out_opts,
                || serde_json::json!({ "success": false, "error": msg }),
                None::<fn()>,
                || error(&msg),
            );
            EXIT_ERROR
        }
    }
}
