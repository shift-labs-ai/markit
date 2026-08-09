mod commands;
mod config;
mod converters;
mod discover_markdown_source;
mod markit;
mod plugins;
mod providers;
mod types;
mod utils;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::commands::convert::{convert, ConvertOptions};
use crate::commands::formats::formats;
use crate::commands::init::init;
use crate::commands::config::{config_get, config_set, config_show};
use crate::commands::onboard::onboard;
use crate::utils::output::{error, OutputOptions};

// clap renders "--version" as "{name} {version}", so keep the bare semver here;
// the CLI output is "markit 0.5.3", matching the TS `markit ${version}` string.
const VERSION: &str = "0.5.3";

#[derive(Parser)]
#[command(
    name = "markit",
    about = "Convert anything to markdown.",
    version = VERSION,
    after_help = r#"Examples:
  $ markit report.pdf                  Convert a PDF to markdown
  $ markit document.docx -o doc.md     Convert DOCX, write to file
  $ markit https://example.com         Convert a web page
  $ markit photo.jpg                    Extract EXIF + AI description
  $ markit recording.mp3               Metadata + transcription
  $ cat file.pdf | markit -            Read from stdin
  $ markit init                        Create .markit/ config
  $ markit config show                 Show LLM settings

Docs: https://github.com/Michaelliv/markit"#
)]
struct Cli {
    /// File path, URL, or - for stdin (default command: convert)
    source: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Raw markdown only, no decoration
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Extra instructions for image description
    #[arg(short, long, global = true)]
    prompt: Option<String>,

    /// Write to file instead of stdout
    #[arg(short, long, global = true)]
    output: Option<String>,

    /// Extract images to this directory
    #[arg(short = 'i', long = "image-dir", global = true)]
    image_dir: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert a file or URL to markdown
    #[command(alias = "c")]
    Convert {
        /// File path, URL, or - for stdin
        source: String,

        /// Write to file instead of stdout
        #[arg(short, long)]
        output: Option<String>,
    },
    /// List supported formats
    Formats,
    /// Create .markit/ config directory
    Init,
    /// Manage markit configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Manage plugins
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// Add markit instructions to CLAUDE.md or AGENTS.md
    Onboard,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Show current configuration
    Show,
    /// Get a config value
    Get {
        /// Config key (e.g. llm.provider)
        key: String,
    },
    /// Set a config value (secrets read from stdin if no value given)
    Set {
        /// Config key (e.g. llm.apiKey)
        key: String,
        /// Value to set
        value: Option<String>,
    },
}

#[derive(Subcommand)]
enum PluginAction {
    /// Install a plugin (npm:pkg, git:url, or local path)
    Install {
        /// Plugin source
        source: String,
    },
    /// Remove an installed plugin
    Remove {
        /// Plugin name
        name: String,
    },
    /// List installed plugins
    List,
}

const NO_ARGS_HELP: &str = "markit — convert anything to markdown

Usage:  markit <file-or-url> [options]

Examples:
  $ markit report.pdf
  $ markit document.docx -o doc.md
  $ markit https://example.com

Commands:
  markit init        Create .markit/ config directory
  markit config      Manage settings (LLM, API keys)
  markit formats     List supported formats
  markit onboard     Add instructions to CLAUDE.md

Run markit --help for all options.
Docs: https://github.com/Michaelliv/markit";

const KNOWN_COMMANDS: &[&str] = &[
    "convert", "formats", "onboard", "help", "init", "config", "plugin",
];

fn levenshtein(a: &str, b: &str) -> usize {
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }
    for (i, ac) in a.chars().enumerate() {
        for (j, bc) in b.chars().enumerate() {
            let cost = if ac != bc { 1 } else { 0 };
            dp[i + 1][j + 1] = (dp[i][j + 1] + 1)
                .min(dp[i + 1][j] + 1)
                .min(dp[i][j] + cost);
        }
    }
    dp[m][n]
}

fn main() -> ExitCode {
    // No args → show concise help
    let args: Vec<String> = std::env::args().collect();
    if args.len() <= 1 {
        println!("{}", NO_ARGS_HELP);
        return ExitCode::SUCCESS;
    }

    // Pre-parse: check for typos against known subcommands before clap parses.
    // This matches the TS behavior of program.on("command:*", ...) which handles
    // unknown commands that look like typos of known ones.
    if args.len() >= 2 {
        let first_arg = &args[1];
        // Only check if it doesn't look like a flag, file path, or URL
        if !first_arg.starts_with('-')
            && !first_arg.contains('/')
            && !first_arg.contains('.')
            && !first_arg.starts_with("http")
        {
            // Check if it's NOT a known command/alias — if so check for typos
            let is_known = matches!(
                first_arg.as_str(),
                "convert" | "c" | "formats" | "init" | "config" | "plugin" | "onboard" | "help"
            );
            if !is_known {
                let close: Vec<&&str> = KNOWN_COMMANDS
                    .iter()
                    .filter(|c| levenshtein(first_arg, c) <= 2 && first_arg.as_str() != **c)
                    .collect();
                if !close.is_empty() {
                    error(&format!(
                        "Unknown command '{}'. Did you mean '{}'?",
                        first_arg, close[0]
                    ));
                    return ExitCode::from(1);
                }
            }
        }
    }

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Formats) => {
            let opts = OutputOptions {
                json: cli.json,
                quiet: cli.quiet,
            };
            formats(&opts);
            ExitCode::SUCCESS
        }
        Some(Commands::Init) => {
            let opts = OutputOptions {
                json: cli.json,
                quiet: cli.quiet,
            };
            init(&opts);
            ExitCode::SUCCESS
        }
        Some(Commands::Config { action }) => {
            let opts = OutputOptions {
                json: cli.json,
                quiet: cli.quiet,
            };
            match action {
                ConfigAction::Show => config_show(&opts),
                ConfigAction::Get { key } => config_get(&key, &opts),
                ConfigAction::Set { key, value } => {
                    config_set(&key, value.as_deref(), &opts)
                }
            }
            ExitCode::SUCCESS
        }
        Some(Commands::Plugin { action }) => {
            let result = match action {
                PluginAction::Install { source } => {
                    crate::commands::plugin::install(&source, cli.json, cli.quiet)
                }
                PluginAction::Remove { name } => {
                    crate::commands::plugin::remove(&name, cli.json, cli.quiet)
                }
                PluginAction::List => {
                    crate::commands::plugin::list(cli.json, cli.quiet)
                }
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    error(&e.to_string());
                    ExitCode::from(1)
                }
            }
        }
        Some(Commands::Onboard) => {
            let opts = OutputOptions {
                json: cli.json,
                quiet: cli.quiet,
            };
            onboard(&opts);
            ExitCode::SUCCESS
        }
        Some(Commands::Convert { source, output: out_override }) => {
            let code = convert(
                &source,
                &ConvertOptions {
                    json: cli.json,
                    quiet: cli.quiet,
                    output_file: out_override.or(cli.output),
                    prompt: cli.prompt,
                    image_dir: cli.image_dir,
                },
            );
            ExitCode::from(code)
        }
        None => {
            // Default behavior: treat source as convert target
            match cli.source {
                Some(source) => {
                    let code = convert(
                        &source,
                        &ConvertOptions {
                            json: cli.json,
                            quiet: cli.quiet,
                            output_file: cli.output,
                            prompt: cli.prompt,
                            image_dir: cli.image_dir,
                        },
                    );
                    ExitCode::from(code)
                }
                None => {
                    println!("{}", NO_ARGS_HELP);
                    ExitCode::SUCCESS
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_identical_strings() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn levenshtein_single_edit() {
        assert_eq!(levenshtein("cat", "bat"), 1);
    }

    #[test]
    fn levenshtein_insertion() {
        assert_eq!(levenshtein("cat", "cats"), 1);
    }

    #[test]
    fn levenshtein_deletion() {
        assert_eq!(levenshtein("cats", "cat"), 1);
    }

    #[test]
    fn levenshtein_empty_strings() {
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn levenshtein_one_empty() {
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn levenshtein_typo_formts_formats() {
        assert_eq!(levenshtein("formts", "formats"), 1);
    }

    #[test]
    fn levenshtein_typo_formas_formats() {
        assert_eq!(levenshtein("formas", "formats"), 1);
    }

    #[test]
    fn levenshtein_typo_convet_convert() {
        assert_eq!(levenshtein("convet", "convert"), 1);
    }

    #[test]
    fn levenshtein_typo_inot_init() {
        assert_eq!(levenshtein("inot", "init"), 1);
    }

    #[test]
    fn levenshtein_too_far() {
        assert!(levenshtein("xyz", "formats") > 2);
    }

    #[test]
    fn known_commands_match_ts() {
        assert_eq!(
            KNOWN_COMMANDS,
            &["convert", "formats", "onboard", "help", "init", "config", "plugin"]
        );
    }

    #[test]
    fn no_args_help_starts_correctly() {
        assert!(NO_ARGS_HELP.starts_with("markit — convert anything to markdown"));
    }

    #[test]
    fn no_args_help_contains_key_lines() {
        assert!(NO_ARGS_HELP.contains("Usage:  markit <file-or-url> [options]"));
        assert!(NO_ARGS_HELP.contains("markit init"));
        assert!(NO_ARGS_HELP.contains("markit config"));
        assert!(NO_ARGS_HELP.contains("markit formats"));
        assert!(NO_ARGS_HELP.contains("markit onboard"));
        assert!(NO_ARGS_HELP.contains("Run markit --help for all options."));
        assert!(NO_ARGS_HELP.contains("Docs: https://github.com/Michaelliv/markit"));
    }

    #[test]
    fn version_string_matches_ts() {
        assert_eq!(VERSION, "0.5.3");
    }
}
