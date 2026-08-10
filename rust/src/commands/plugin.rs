//! CLI plugin commands — mirrors src/commands/plugin.ts.
//!
//! install / remove / list with triple output (json / quiet / human).

use anyhow::Result;

use crate::plugins::installer::{install_plugin, list_installed, remove_plugin};

/// Install a plugin from source (npm:pkg, git:url, or local path).
pub fn install(source: &str, json: bool, quiet: bool) -> Result<()> {
    match install_plugin(source) {
        Ok((path, name)) => {
            if json {
                crate::utils::output::json_output(&serde_json::json!({
                    "success": true,
                    "name": name,
                    "path": path,
                }));
            } else if !quiet {
                println!("\u{2714} Installed {name}");
                println!("  {path}");
            }
            Ok(())
        }
        Err(err) => {
            let msg = err.to_string();
            if json {
                crate::utils::output::json_output(&serde_json::json!({
                    "success": false,
                    "error": msg,
                }));
            } else {
                eprintln!("\u{2718} {msg}");
            }
            std::process::exit(1);
        }
    }
}

/// Remove a plugin by name.
pub fn remove(name: &str, json: bool, quiet: bool) -> Result<()> {
    match remove_plugin(name) {
        Ok(true) => {
            if json {
                crate::utils::output::json_output(&serde_json::json!({
                    "success": true,
                    "name": name,
                }));
            } else if !quiet {
                println!("\u{2714} Removed {name}");
            }
            Ok(())
        }
        Ok(false) => {
            if json {
                crate::utils::output::json_output(&serde_json::json!({
                    "success": false,
                    "name": name,
                }));
            } else {
                eprintln!("\u{2718} Plugin '{name}' not found");
            }
            std::process::exit(1);
        }
        Err(err) => {
            let msg = err.to_string();
            if json {
                crate::utils::output::json_output(&serde_json::json!({
                    "success": false,
                    "error": msg,
                }));
            } else {
                eprintln!("\u{2718} {msg}");
            }
            std::process::exit(1);
        }
    }
}

/// List installed plugins.
pub fn list(json: bool, quiet: bool) -> Result<()> {
    let plugins = list_installed();

    if json {
        let items: Vec<serde_json::Value> = plugins
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "type": p.kind,
                    "source": p.source,
                    "path": p.path,
                })
            })
            .collect();
        crate::utils::output::json_output(&serde_json::json!({ "plugins": items }));
    } else if !quiet {
        if plugins.is_empty() {
            println!("  No plugins installed");
        } else {
            println!();
            println!("\x1b[1mInstalled plugins\x1b[0m");
            println!();
            for p in &plugins {
                println!("  {:<20} {} {}", p.name, p.kind, p.source);
            }
            println!();
        }
    }

    Ok(())
}
