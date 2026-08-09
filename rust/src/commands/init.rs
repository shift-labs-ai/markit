use std::fs;

use crate::utils::output::{cmd, hint, output, success, OutputOptions};

const DATA_DIR: &str = ".markit";

pub fn init(options: &OutputOptions) {
    let cwd = std::env::current_dir().unwrap();
    let root = cwd.join(DATA_DIR);

    if root.exists() {
        output(
            options,
            || {
                serde_json::json!({
                    "success": true,
                    "path": root.to_string_lossy(),
                    "message": "already_exists",
                })
            },
            None::<fn()>,
            || success(".markit/ already exists"),
        );
        return;
    }

    fs::create_dir_all(&root).unwrap();

    let config = serde_json::json!({ "llm": {} });
    let config_path = root.join("config.json");
    fs::write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&config).unwrap()),
    )
    .unwrap();

    output(
        options,
        || {
            serde_json::json!({
                "success": true,
                "path": root.to_string_lossy().to_string(),
            })
        },
        None::<fn()>,
        || {
            success(&format!("Created .markit/ in {}", cwd.display()));
            hint("Set your API key for image/audio AI features:");
            println!("  {}", cmd("export OPENAI_API_KEY=sk-..."));
            hint("Or configure directly:");
            println!("  {}", cmd("markit config set llm.apiKey sk-..."));
        },
    );
}
