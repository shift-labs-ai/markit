//! Port of src/config.ts — config discovery and loading.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const DATA_DIR: &str = ".markit";
const CONFIG_FILE: &str = "config.json";

/// LLM sub-section of the config file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmConfig {
    /// Provider name: "openai" (default), "anthropic", or any registered provider.
    pub provider: Option<String>,
    /// API base URL (overrides provider default).
    #[serde(rename = "apiBase", skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// API key — prefer env vars over storing here.
    #[serde(rename = "apiKey", skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Model override (overrides provider default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Transcription model override.
    #[serde(rename = "transcriptionModel", skip_serializing_if = "Option::is_none")]
    pub transcription_model: Option<String>,
}

/// Top-level config structure persisted in .markit/config.json.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MarkitConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmConfig>,
}

/// Walk up from cwd to find .markit/ directory.
/// Mirrors the TS `findConfigDir`.
pub fn find_config_dir() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(DATA_DIR);
        if candidate.exists() {
            return Some(candidate);
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Load config from .markit/config.json.
/// Returns an empty config if the file or directory doesn't exist.
pub fn load_config() -> MarkitConfig {
    let Some(config_dir) = find_config_dir() else {
        return MarkitConfig::default();
    };

    let config_file = config_dir.join(CONFIG_FILE);
    if !config_file.exists() {
        return MarkitConfig::default();
    }

    let raw = match fs::read_to_string(&config_file) {
        Ok(s) => s,
        Err(_) => return MarkitConfig::default(),
    };

    serde_json::from_str(&raw).unwrap_or_default()
}

/// Save config to .markit/config.json. Creates .markit/ if needed.
pub fn save_config(config: &MarkitConfig) -> Result<()> {
    let dir = find_config_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join(DATA_DIR));

    if !dir.exists() {
        fs::create_dir_all(&dir)?;
    }

    let json = format!("{}\n", serde_json::to_string_pretty(config)?);
    fs::write(dir.join(CONFIG_FILE), json)?;
    Ok(())
}

/// Load config from a specific root directory (for testing / alternate roots).
pub fn load_config_from(root: &std::path::Path) -> MarkitConfig {
    let config_file = root.join(DATA_DIR).join(CONFIG_FILE);
    if !config_file.exists() {
        return MarkitConfig::default();
    }
    let raw = match fs::read_to_string(&config_file) {
        Ok(s) => s,
        Err(_) => return MarkitConfig::default(),
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a unique temp directory for this test run.
    fn make_test_root(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("markit-config-test-{}-{}", std::process::id(), suffix));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create a .markit/config.json inside root with the given JSON content.
    fn write_config(root: &PathBuf, json: &str) {
        let d = root.join(".markit");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("config.json"), json).unwrap();
    }

    #[test]
    fn empty_config_when_no_file() {
        let root = make_test_root("no-file");
        let cfg = load_config_from(&root);
        assert_eq!(cfg, MarkitConfig::default());
    }

    #[test]
    fn parses_provider_field() {
        let root = make_test_root("provider");
        write_config(&root, r#"{ "llm": { "provider": "anthropic" } }"#);
        let cfg = load_config_from(&root);
        assert_eq!(cfg.llm.unwrap().provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn parses_api_key() {
        let root = make_test_root("apikey");
        write_config(&root, r#"{ "llm": { "apiKey": "sk-abc123" } }"#);
        let cfg = load_config_from(&root);
        assert_eq!(cfg.llm.unwrap().api_key.as_deref(), Some("sk-abc123"));
    }

    #[test]
    fn parses_api_base() {
        let root = make_test_root("apibase");
        write_config(&root, r#"{ "llm": { "apiBase": "https://custom.api.com/v1" } }"#);
        let cfg = load_config_from(&root);
        assert_eq!(
            cfg.llm.unwrap().api_base.as_deref(),
            Some("https://custom.api.com/v1")
        );
    }

    #[test]
    fn parses_model_fields() {
        let root = make_test_root("models");
        write_config(
            &root,
            r#"{ "llm": { "model": "gpt-4", "transcriptionModel": "whisper-1" } }"#,
        );
        let cfg = load_config_from(&root);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.model.as_deref(), Some("gpt-4"));
        assert_eq!(llm.transcription_model.as_deref(), Some("whisper-1"));
    }

    #[test]
    fn parses_full_config() {
        let root = make_test_root("full");
        write_config(
            &root,
            r#"{
  "llm": {
    "provider": "openai",
    "apiBase": "https://api.openai.com/v1",
    "apiKey": "sk-test",
    "model": "gpt-4.1-nano",
    "transcriptionModel": "gpt-4o-mini-transcribe"
  }
}"#,
        );
        let cfg = load_config_from(&root);
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.provider.as_deref(), Some("openai"));
        assert_eq!(llm.api_base.as_deref(), Some("https://api.openai.com/v1"));
        assert_eq!(llm.api_key.as_deref(), Some("sk-test"));
        assert_eq!(llm.model.as_deref(), Some("gpt-4.1-nano"));
        assert_eq!(
            llm.transcription_model.as_deref(),
            Some("gpt-4o-mini-transcribe")
        );
    }

    #[test]
    fn returns_default_on_missing_config_dir() {
        let root = make_test_root("missing-dir");
        // no .markit/ directory created
        let cfg = load_config_from(&root);
        assert_eq!(cfg, MarkitConfig::default());
        assert!(cfg.llm.is_none());
    }

    #[test]
    fn returns_default_on_invalid_json() {
        let root = make_test_root("bad-json");
        write_config(&root, "not json at all {{{");
        let cfg = load_config_from(&root);
        assert_eq!(cfg, MarkitConfig::default());
    }

    #[test]
    fn ignores_unknown_fields() {
        let root = make_test_root("unknown-fields");
        write_config(
            &root,
            r#"{ "llm": { "provider": "openai", "unknownField": true } }"#,
        );
        let cfg = load_config_from(&root);
        assert_eq!(cfg.llm.unwrap().provider.as_deref(), Some("openai"));
    }

    #[test]
    fn roundtrip_serialization() {
        let config = MarkitConfig {
            llm: Some(LlmConfig {
                provider: Some("anthropic".to_string()),
                api_key: Some("key123".to_string()),
                model: Some("claude-haiku-4-5".to_string()),
                api_base: None,
                transcription_model: None,
            }),
        };
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: MarkitConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn serializes_with_camel_case_keys() {
        let config = MarkitConfig {
            llm: Some(LlmConfig {
                api_base: Some("https://example.com".to_string()),
                api_key: Some("k".to_string()),
                transcription_model: Some("m".to_string()),
                ..Default::default()
            }),
        };
        let json = serde_json::to_string(&config).unwrap();
        // Keys must be camelCase to match the TS config file format
        assert!(json.contains("\"apiBase\""), "expected apiBase, got: {json}");
        assert!(json.contains("\"apiKey\""), "expected apiKey, got: {json}");
        assert!(
            json.contains("\"transcriptionModel\""),
            "expected transcriptionModel, got: {json}"
        );
    }
}
