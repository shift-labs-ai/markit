use std::fs;

use crate::utils::output::{output, success, OutputOptions};

const INSTRUCTIONS: &str = r#"<mill>
Use `markit` to convert files and URLs to markdown. Supports PDF, DOCX, HTML, XLSX, CSV, JSON, and plain text.

<commands>
- `markit <file-or-url>` - convert to markdown (stdout)
- `markit <file-or-url> -o output.md` - convert to file
- `markit formats` - list supported formats
</commands>

<rules>
- Use `--json` flag to get structured output for parsing
- Use `-q` to get raw markdown without formatting
- Pipe output directly: `markit report.pdf | other-tool`
</rules>
</markit>"#;

const MARKER: &str = "<mill>";

pub fn onboard(options: &OutputOptions) {
    let cwd = std::env::current_dir().unwrap();
    let claude_md = cwd.join("CLAUDE.md");
    let agents_md = cwd.join("AGENTS.md");

    let target_file = if claude_md.exists() {
        claude_md
    } else if agents_md.exists() {
        agents_md
    } else {
        claude_md
    };

    let existing_content = if target_file.exists() {
        fs::read_to_string(&target_file).unwrap_or_default()
    } else {
        String::new()
    };

    if existing_content.contains(MARKER) {
        output(
            options,
            || {
                serde_json::json!({
                    "success": true,
                    "file": target_file.to_string_lossy(),
                    "message": "already_onboarded",
                })
            },
            None::<fn()>,
            || success(&format!("Already onboarded ({})", target_file.display())),
        );
        return;
    }

    let new_content = if !existing_content.is_empty() {
        format!("{}\n\n{}\n", existing_content.trim_end(), INSTRUCTIONS)
    } else {
        format!("{}\n", INSTRUCTIONS)
    };

    fs::write(&target_file, new_content).unwrap();

    output(
        options,
        || {
            serde_json::json!({
                "success": true,
                "file": target_file.to_string_lossy(),
            })
        },
        None::<fn()>,
        || success(&format!("Added markit instructions to {}", target_file.display())),
    );
}
