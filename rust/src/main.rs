mod converters;
mod discover_markdown_source;
mod markit;
mod types;
mod utils;

use std::io::Read;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::markit::Markit;
use crate::types::{ConversionResult, MarkitOptions, StreamInfo};

#[derive(Parser)]
#[command(name = "markit", version, about = "Convert anything to markdown.")]
struct Cli {
    /// File path, URL, or - for stdin
    source: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Raw markdown only, no decoration
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Write to file instead of stdout
    #[arg(short, long, global = true)]
    output: Option<String>,

    /// Extra instructions for image description
    #[arg(short, long, global = true)]
    prompt: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert a file or URL to markdown
    #[command(alias = "c")]
    Convert {
        /// File path, URL, or - for stdin
        source: String,
    },
    /// List supported formats
    Formats,
}

const EXIT_ERROR: u8 = 1;
const EXIT_USER_ERROR: u8 = 2; // Invalid input, missing args
const EXIT_UNSUPPORTED: u8 = 3; // Unsupported format

fn main() -> ExitCode {
    let cli = Cli::parse();

    let source = match (&cli.command, &cli.source) {
        (Some(Commands::Formats), _) => {
            print_formats(cli.json);
            return ExitCode::SUCCESS;
        }
        (Some(Commands::Convert { source }), _) => source.clone(),
        (None, Some(source)) => source.clone(),
        (None, None) => {
            eprintln!("Usage: markit <file-or-url> [options]");
            return ExitCode::from(EXIT_USER_ERROR);
        }
    };

    let options = MarkitOptions { prompt: cli.prompt.clone() };
    let markit = Markit::new(options);

    let result = if source == "-" {
        let mut buffer = Vec::new();
        if let Err(err) = std::io::stdin().read_to_end(&mut buffer) {
            eprintln!("Error: {err}");
            return ExitCode::FAILURE;
        }
        markit.convert(&buffer, &StreamInfo::default())
    } else if source.starts_with("http://") || source.starts_with("https://") {
        Err(anyhow::anyhow!("URL conversion not yet implemented in the Rust port"))
    } else {
        markit.convert_file(&source)
    };

    match result {
        Ok(res) => emit(&res, &source, &cli),
        Err(err) => {
            let msg = err.to_string();
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({ "success": false, "error": msg })
                );
            } else {
                eprintln!("✗ {msg}");
            }
            let code = if msg.contains("Unsupported format") {
                EXIT_UNSUPPORTED
            } else {
                EXIT_ERROR
            };
            ExitCode::from(code)
        }
    }
}

fn emit(res: &ConversionResult, source: &str, cli: &Cli) -> ExitCode {
    match &cli.output {
        Some(path) => {
            if let Err(err) = std::fs::write(path, &res.markdown) {
                eprintln!("✗ Error writing {path}: {err}");
                return ExitCode::from(EXIT_ERROR);
            }
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "success": true,
                        "source": source,
                        "output": path,
                        "title": res.title,
                        "length": res.markdown.chars().count(),
                    })
                );
            } else if !cli.quiet {
                println!("✓ Converted → {path}");
                println!("  {} chars", res.markdown.chars().count());
            }
        }
        None => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "success": true,
                        "source": source,
                        "title": res.title,
                        "markdown": res.markdown,
                    })
                );
            } else {
                // Raw markdown, no trailing newline added — matches the TS CLI.
                print!("{}", res.markdown);
            }
        }
    }
    ExitCode::SUCCESS
}

fn print_formats(json: bool) {
    let formats = [
        ("csv", "CSV / TSV tables"),
        ("json", "JSON (pretty-printed)"),
        ("yaml", "YAML"),
        ("xml", "XML / SVG"),
        ("plain-text", "Plain text, markdown, source code"),
    ];
    if json {
        let list: Vec<_> = formats
            .iter()
            .map(|(name, desc)| serde_json::json!({ "name": name, "description": desc }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&list).unwrap());
    } else {
        println!("Supported formats:");
        for (name, desc) in formats {
            println!("  {name:<12} {desc}");
        }
    }
}
