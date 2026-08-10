use std::io::{self, IsTerminal, Write};

/// Whether ANSI color codes should be emitted.
/// Respects NO_COLOR env var and non-TTY stdout (like chalk does).
fn use_color() -> bool {
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    io::stdout().is_terminal()
}

// ANSI escape helpers
#[allow(dead_code)] // used by info(), which exists for TS API parity
fn blue(s: &str) -> String {
    if use_color() {
        format!("\x1b[34m{}\x1b[0m", s)
    } else {
        s.to_string()
    }
}

fn green(s: &str) -> String {
    if use_color() {
        format!("\x1b[32m{}\x1b[0m", s)
    } else {
        s.to_string()
    }
}

fn red(s: &str) -> String {
    if use_color() {
        format!("\x1b[31m{}\x1b[0m", s)
    } else {
        s.to_string()
    }
}

fn yellow(s: &str) -> String {
    if use_color() {
        format!("\x1b[33m{}\x1b[0m", s)
    } else {
        s.to_string()
    }
}

fn cyan_str(s: &str) -> String {
    if use_color() {
        format!("\x1b[36m{}\x1b[0m", s)
    } else {
        s.to_string()
    }
}

fn bold_ansi(s: &str) -> String {
    if use_color() {
        format!("\x1b[1m{}\x1b[0m", s)
    } else {
        s.to_string()
    }
}

fn dim_ansi(s: &str) -> String {
    if use_color() {
        format!("\x1b[2m{}\x1b[0m", s)
    } else {
        s.to_string()
    }
}

// Public formatting functions matching chalk usage in TS
pub fn bold(s: &str) -> String {
    bold_ansi(s)
}

pub fn dim(s: &str) -> String {
    dim_ansi(s)
}

// Public output functions matching TS
pub fn success(msg: &str) {
    println!("{} {}", green("✓"), msg);
}

#[allow(dead_code)] // API parity with src/utils/output.ts info()
pub fn info(msg: &str) {
    println!("{} {}", blue("ℹ"), msg);
}

#[allow(dead_code)]
pub fn warn(msg: &str) {
    println!("{} {}", yellow("⚠"), msg);
}

pub fn error(msg: &str) {
    eprintln!("{} {}", red("✗"), msg);
}

#[allow(dead_code)]
pub fn next_step(command: &str) {
    println!("  {}", cyan_str(command));
}

#[allow(dead_code)]
pub fn header(title: &str) {
    println!();
    println!("{}", bold_ansi(title));
    println!();
}

pub fn json_output(data: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(data).unwrap());
}

/// Output options matching the TS OutputOptions interface
pub struct OutputOptions {
    pub json: bool,
    pub quiet: bool,
}

/// Triple output handler matching the TS output() function.
/// Calls the appropriate handler based on options.
pub fn output<J, Q, H>(options: &OutputOptions, json_fn: J, quiet_fn: Option<Q>, human_fn: H)
where
    J: FnOnce() -> serde_json::Value,
    Q: FnOnce(),
    H: FnOnce(),
{
    if options.json {
        json_output(&json_fn());
    } else if options.quiet {
        // TS: quiet WITHOUT a quiet handler falls through to the human
        // handler (see src/utils/output.ts output()).
        match quiet_fn {
            Some(q) => q(),
            None => human_fn(),
        }
    } else {
        human_fn();
    }
}

/// Write raw bytes to stdout without trailing newline.
pub fn write_stdout(s: &str) {
    let _ = io::stdout().write_all(s.as_bytes());
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_wraps_text_when_color_disabled() {
        // In test environment, stdout is not a terminal, so no ANSI
        let result = bold("hello");
        assert_eq!(result, "hello");
    }

    #[test]
    fn dim_wraps_text_when_color_disabled() {
        let result = dim("faded");
        assert_eq!(result, "faded");
    }
}
