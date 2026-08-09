use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

use anyhow::Result;

use crate::utils::output::{bold, dim, error, json_output, output, success, OutputOptions};

// Exit codes matching TS
const EXIT_ERROR: u8 = 1;
const EXIT_USER_ERROR: u8 = 2;

/// Find .markit/ config directory by walking up from cwd.
/// TODO(config): use crate::config::find_config_dir when landed.
fn find_config_dir() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".markit");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Load config from .markit/config.json.
/// TODO(config): use crate::config::load_config when landed.
fn load_config() -> serde_json::Value {
    if let Some(config_dir) = find_config_dir() {
        let path = config_dir.join("config.json");
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Ok(val) = serde_json::from_str(&contents) {
                return val;
            }
        }
    }
    serde_json::json!({ "llm": {} })
}

/// Save config to .markit/config.json.
fn save_config(config: &serde_json::Value) -> Result<()> {
    let config_dir =
        find_config_dir().ok_or_else(|| anyhow::anyhow!("No .markit/ directory"))?;
    let path = config_dir.join("config.json");
    fs::write(&path, format!("{}\n", serde_json::to_string_pretty(config)?))?;
    Ok(())
}

fn get_nested_value<'a>(obj: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = obj;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    Some(current)
}

fn set_nested_value(obj: &mut serde_json::Value, path: &str, value: serde_json::Value) {
    let keys: Vec<&str> = path.split('.').collect();
    let mut current = obj;
    for key in &keys[..keys.len() - 1] {
        if !current.get(key).map_or(false, |v| v.is_object()) {
            current[*key] = serde_json::json!({});
        }
        current = current.get_mut(key).unwrap();
    }
    current[keys[keys.len() - 1]] = value;
}

/// Static provider metadata mirroring src/providers/index.ts registry order.
struct ProviderMeta {
    env_keys: &'static [&'static str],
    default_base: &'static str,
    default_model: &'static str,
    default_transcription_model: Option<&'static str>,
}

fn provider_meta(name: &str) -> Option<ProviderMeta> {
    match name {
        "openai" => Some(ProviderMeta {
            env_keys: crate::providers::openai::ENV_KEYS,
            default_base: crate::providers::openai::DEFAULT_BASE,
            default_model: crate::providers::openai::DEFAULT_MODEL,
            default_transcription_model: Some(crate::providers::openai::DEFAULT_TRANSCRIPTION_MODEL),
        }),
        "anthropic" => Some(ProviderMeta {
            env_keys: crate::providers::anthropic::ENV_KEYS,
            default_base: crate::providers::anthropic::DEFAULT_BASE,
            default_model: crate::providers::anthropic::DEFAULT_MODEL,
            default_transcription_model: None,
        }),
        _ => None,
    }
}

/// Registry order from src/providers/index.ts (object key order).
const PROVIDER_NAMES: &[&str] = &["openai", "anthropic"];

pub fn config_show(options: &OutputOptions) {
    let config = load_config();
    let config_dir = find_config_dir();
    let provider_name = config
        .get("llm")
        .and_then(|llm| llm.get("provider"))
        .and_then(|v| v.as_str())
        .unwrap_or("openai")
        .to_string();
    let meta = provider_meta(&provider_name);

    if options.json {
        json_output(&serde_json::json!({
            "configDir": config_dir.map(|p| p.to_string_lossy().to_string()),
            "config": config,
            "providers": PROVIDER_NAMES,
        }));
        return;
    }

    // Human output — mirrors src/commands/config.ts configShow exactly.
    println!();
    println!("{}", bold("Configuration"));
    println!();
    if let Some(dir) = config_dir {
        println!("  {} {}/config.json", dim("config:"), dir.display());
    } else {
        println!("  {} none (run 'markit init')", dim("config:"));
    }
    println!();
    println!("{}", bold("LLM Settings"));
    println!();
    println!("  {} {}", dim("provider:"), provider_name);

    if let Some(meta) = meta {
        let llm = config.get("llm");
        let config_key = llm
            .and_then(|l| l.get("apiKey"))
            .and_then(|v| v.as_str())
            .map(String::from);

        // API key: env vars (in provider priority order) > config file
        let env_hit = meta
            .env_keys
            .iter()
            .find(|k| std::env::var(k).is_ok())
            .copied();
        let api_key = meta
            .env_keys
            .iter()
            .find_map(|k| std::env::var(k).ok())
            .or_else(|| config_key.clone());
        let key_source = env_hit.map(String::from).or_else(|| {
            if config_key.is_some() {
                Some("config".to_string())
            } else {
                None
            }
        });

        match (&api_key, &key_source) {
            (Some(key), Some(source)) => {
                let last4: String = key
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                println!("  {} ***{} ({})", dim("api key:"), last4, source);
            }
            _ => println!("  {} {}", dim("api key:"), dim("not set")),
        }

        let api_base = llm
            .and_then(|l| l.get("apiBase"))
            .and_then(|v| v.as_str())
            .unwrap_or(meta.default_base);
        println!("  {} {}", dim("api base:"), api_base);

        let model = llm
            .and_then(|l| l.get("model"))
            .and_then(|v| v.as_str())
            .unwrap_or(meta.default_model);
        println!("  {} {}", dim("model:"), model);

        if let Some(default_tm) = meta.default_transcription_model {
            let tm = llm
                .and_then(|l| l.get("transcriptionModel"))
                .and_then(|v| v.as_str())
                .unwrap_or(default_tm);
            println!("  {} {}", dim("transcription:"), tm);
        }

        println!("  {} {}", dim("env vars:"), meta.env_keys.join(", "));
    } else {
        println!("  {}", dim("(unknown provider)"));
    }

    println!();
    println!(
        "  {}",
        dim(&format!("Available providers: {}", PROVIDER_NAMES.join(", ")))
    );
    println!();
}

pub fn config_get(key: &str, options: &OutputOptions) {
    let config = load_config();
    let value = get_nested_value(&config, key);

    match value {
        None | Some(serde_json::Value::Null) => {
            output(
                options,
                || serde_json::json!({ "key": key, "value": null }),
                None::<fn()>,
                || error(&format!("Key '{}' not found", key)),
            );
            std::process::exit(EXIT_USER_ERROR as i32);
        }
        Some(val) => {
            output(
                options,
                || serde_json::json!({ "key": key, "value": val }),
                Some(|| {
                    match val {
                        serde_json::Value::String(s) => println!("{}", s),
                        other => println!("{}", other),
                    }
                }),
                || {
                    match val {
                        serde_json::Value::String(s) => println!("{}", s),
                        other => println!("{}", other),
                    }
                },
            );
        }
    }
}

pub fn config_set(key: &str, value: Option<&str>, options: &OutputOptions) {
    if find_config_dir().is_none() {
        output(
            options,
            || {
                serde_json::json!({
                    "success": false,
                    "error": "No .markit/ directory. Run 'markit init'",
                })
            },
            None::<fn()>,
            || error("No .markit/ directory. Run 'markit init' first."),
        );
        std::process::exit(EXIT_ERROR as i32);
    }

    let key_lower = key.to_lowercase();
    let is_secret = key_lower.contains("key")
        || key_lower.contains("secret")
        || key_lower.contains("token");

    let resolved: String = if is_secret && value.is_none() {
        // Read from stdin
        if io::stdin().is_terminal() {
            eprint!("Enter value for {}: ", key);
            let _ = io::stderr().flush();
        }
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).unwrap_or(0);
        let trimmed = buf.trim().to_string();
        if trimmed.is_empty() {
            error("No value provided");
            std::process::exit(EXIT_USER_ERROR as i32);
        }
        trimmed
    } else if is_secret && value.is_some() {
        eprintln!(
            "  {}",
            crate::utils::output::dim(
                "hint: secrets in args leak to shell history. Use: markit config set llm.apiKey < keyfile",
            )
        );
        value.unwrap().to_string()
    } else if value.is_none() {
        error("Missing value. Usage: markit config set <key> <value>");
        std::process::exit(EXIT_USER_ERROR as i32);
    } else {
        value.unwrap().to_string()
    };

    let mut config = load_config();

    // Parse value: true/false/integer/string
    let parsed: serde_json::Value = if resolved == "true" {
        serde_json::Value::Bool(true)
    } else if resolved == "false" {
        serde_json::Value::Bool(false)
    } else if resolved.chars().all(|c| c.is_ascii_digit()) && !resolved.is_empty() {
        if let Ok(n) = resolved.parse::<i64>() {
            serde_json::Value::Number(n.into())
        } else {
            serde_json::Value::String(resolved.clone())
        }
    } else {
        serde_json::Value::String(resolved.clone())
    };

    set_nested_value(&mut config, key, parsed.clone());

    if let Err(e) = save_config(&config) {
        error(&format!("Failed to save config: {}", e));
        std::process::exit(EXIT_ERROR as i32);
    }

    output(
        options,
        || serde_json::json!({ "success": true, "key": key, "value": parsed }),
        None::<fn()>,
        || success(&format!("{} = {}", key, serde_json::to_string(&parsed).unwrap())),
    );
}
