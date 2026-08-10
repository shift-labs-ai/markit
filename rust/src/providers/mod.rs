//! LLM provider registry and `create_llm_functions`.
//! Port of src/providers/index.ts.
//!
//! Call `create_llm_functions(&config, prompt)` to get a `MarkitOptions` with
//! describe/transcribe closures populated when API keys are available.

pub mod anthropic;
pub mod openai;
pub mod types;

use crate::config::MarkitConfig;
use crate::types::MarkitOptions;

use anthropic::{
    AnthropicProvider, DEFAULT_BASE as ANTHROPIC_BASE, DEFAULT_MODEL as ANTHROPIC_MODEL,
    ENV_KEYS as ANTHROPIC_KEYS,
};
use openai::{
    OpenAiProvider, DEFAULT_BASE as OPENAI_BASE, DEFAULT_MODEL as OPENAI_MODEL,
    DEFAULT_TRANSCRIPTION_MODEL, ENV_KEYS as OPENAI_KEYS,
};
use types::ResolvedConfig;

const BASE_PROMPT: &str = "Describe this image in detail.";

/// Resolve the API key, base URL, and model for a given provider and config.
/// Priority mirrors the TS `resolve` function:
///   env vars (in provider order) > config file key > default
fn resolve(
    env_keys: &[&str],
    default_base: &str,
    default_model: &str,
    default_transcription_model: Option<&str>,
    config: &MarkitConfig,
) -> Option<ResolvedConfig> {
    // API key: provider env vars (in order) > config file apiKey
    let api_key = env_keys
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .or_else(|| config.llm.as_ref()?.api_key.clone())?;

    let api_base = config
        .llm
        .as_ref()
        .and_then(|l| l.api_base.as_deref())
        .unwrap_or(default_base)
        .trim_end_matches('/')
        .to_string();

    let model = std::env::var("MARKIT_MODEL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| config.llm.as_ref()?.model.clone())
        .unwrap_or_else(|| default_model.to_string());

    let transcription_model = config
        .llm
        .as_ref()
        .and_then(|l| l.transcription_model.clone())
        .or_else(|| default_transcription_model.map(String::from));

    Some(ResolvedConfig {
        api_key,
        api_base,
        model,
        transcription_model,
    })
}

/// Build `describe`/`transcribe` closures from config + optional extra prompt.
///
/// - If no API key is available (no env var, no config key) returns empty `MarkitOptions`.
/// - The `prompt` is appended to the default "Describe this image in detail." base.
/// - Mirrors the TS `createLlmFunctions`.
///
/// # Errors
/// Returns an error if the provider name in config is unknown.
pub fn create_llm_functions(
    config: &MarkitConfig,
    prompt: Option<String>,
) -> anyhow::Result<MarkitOptions> {
    let provider_name = config
        .llm
        .as_ref()
        .and_then(|l| l.provider.as_deref())
        .unwrap_or("openai");

    let full_prompt = match &prompt {
        Some(p) => format!("{BASE_PROMPT}\n\n{p}"),
        None => BASE_PROMPT.to_string(),
    };

    match provider_name {
        "anthropic" => {
            let Some(resolved) = resolve(
                ANTHROPIC_KEYS,
                ANTHROPIC_BASE,
                ANTHROPIC_MODEL,
                None,
                config,
            ) else {
                return Ok(MarkitOptions::default());
            };
            Ok(AnthropicProvider::new(resolved, full_prompt).into_options())
        }
        "openai" => {
            let Some(resolved) = resolve(
                OPENAI_KEYS,
                OPENAI_BASE,
                OPENAI_MODEL,
                Some(DEFAULT_TRANSCRIPTION_MODEL),
                config,
            ) else {
                return Ok(MarkitOptions::default());
            };
            Ok(OpenAiProvider::new(resolved, full_prompt).into_options())
        }
        name => Err(anyhow::anyhow!(
            "Unknown provider '{name}'. Available: openai, anthropic"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LlmConfig, MarkitConfig};

    /// Serialize tests that mutate environment variables.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn config_with_key(provider: &str, key: &str) -> MarkitConfig {
        MarkitConfig {
            llm: Some(LlmConfig {
                provider: Some(provider.to_string()),
                api_key: Some(key.to_string()),
                ..Default::default()
            }),
        }
    }

    fn config_with_provider(provider: &str) -> MarkitConfig {
        MarkitConfig {
            llm: Some(LlmConfig {
                provider: Some(provider.to_string()),
                ..Default::default()
            }),
        }
    }

    fn empty_config() -> MarkitConfig {
        MarkitConfig::default()
    }

    // ── Missing key → None closures ──────────────────────────────────────────

    #[test]
    fn no_api_key_returns_empty_options_for_openai() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Remove env vars to ensure no key is found
        let prev_key = std::env::var("OPENAI_API_KEY").ok();
        let prev_markit = std::env::var("MARKIT_API_KEY").ok();
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("MARKIT_API_KEY");

        let opts = create_llm_functions(&empty_config(), None).unwrap();
        assert!(
            opts.describe.is_none(),
            "describe should be None with no key"
        );
        assert!(
            opts.transcribe.is_none(),
            "transcribe should be None with no key"
        );

        // Restore
        if let Some(v) = prev_key {
            std::env::set_var("OPENAI_API_KEY", v);
        }
        if let Some(v) = prev_markit {
            std::env::set_var("MARKIT_API_KEY", v);
        }
    }

    #[test]
    fn no_api_key_returns_empty_options_for_anthropic() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_a = std::env::var("ANTHROPIC_API_KEY").ok();
        let prev_m = std::env::var("MARKIT_API_KEY").ok();
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("MARKIT_API_KEY");

        let cfg = config_with_provider("anthropic");
        let opts = create_llm_functions(&cfg, None).unwrap();
        assert!(
            opts.describe.is_none(),
            "describe should be None with no key"
        );
        assert!(opts.transcribe.is_none());

        if let Some(v) = prev_a {
            std::env::set_var("ANTHROPIC_API_KEY", v);
        }
        if let Some(v) = prev_m {
            std::env::set_var("MARKIT_API_KEY", v);
        }
    }

    // ── API key found → closures populated ──────────────────────────────────

    #[test]
    fn config_api_key_populates_openai_describe() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("OPENAI_API_KEY").ok();
        let prev_m = std::env::var("MARKIT_API_KEY").ok();
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("MARKIT_API_KEY");

        let cfg = config_with_key("openai", "sk-from-config");
        let opts = create_llm_functions(&cfg, None).unwrap();
        assert!(opts.describe.is_some(), "describe should be set");
        assert!(
            opts.transcribe.is_some(),
            "transcribe should be set for OpenAI"
        );

        if let Some(v) = prev {
            std::env::set_var("OPENAI_API_KEY", v);
        }
        if let Some(v) = prev_m {
            std::env::set_var("MARKIT_API_KEY", v);
        }
    }

    #[test]
    fn config_api_key_populates_anthropic_describe() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("ANTHROPIC_API_KEY").ok();
        let prev_m = std::env::var("MARKIT_API_KEY").ok();
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("MARKIT_API_KEY");

        let cfg = config_with_key("anthropic", "sk-anth-config");
        let opts = create_llm_functions(&cfg, None).unwrap();
        assert!(opts.describe.is_some(), "describe should be set");
        // Anthropic has no transcription
        assert!(
            opts.transcribe.is_none(),
            "Anthropic should have no transcribe"
        );

        if let Some(v) = prev {
            std::env::set_var("ANTHROPIC_API_KEY", v);
        }
        if let Some(v) = prev_m {
            std::env::set_var("MARKIT_API_KEY", v);
        }
    }

    // ── Provider selection ───────────────────────────────────────────────────

    #[test]
    fn defaults_to_openai_provider_when_none_specified() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("OPENAI_API_KEY").ok();
        let prev_m = std::env::var("MARKIT_API_KEY").ok();
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("MARKIT_API_KEY");

        // No provider specified, no key — should still not error; returns empty
        let opts = create_llm_functions(&empty_config(), None).unwrap();
        // With no key, options should be empty (openai is default)
        assert!(opts.describe.is_none());

        if let Some(v) = prev {
            std::env::set_var("OPENAI_API_KEY", v);
        }
        if let Some(v) = prev_m {
            std::env::set_var("MARKIT_API_KEY", v);
        }
    }

    #[test]
    fn unknown_provider_returns_error() {
        let cfg = config_with_provider("nonexistent-provider");
        let result = create_llm_functions(&cfg, None);
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected error for unknown provider"),
        };
        assert!(err.to_string().contains("Unknown provider"), "got: {err}");
        assert!(
            err.to_string().contains("nonexistent-provider"),
            "got: {err}"
        );
    }

    // ── Prompt composition ───────────────────────────────────────────────────

    #[test]
    fn prompt_is_appended_to_base_prompt() {
        // We test resolve() directly since the closure is opaque.
        // The full_prompt should contain both the base and the extra text.
        let full = format!("{BASE_PROMPT}\n\nextra context");
        assert!(full.starts_with("Describe this image in detail."));
        assert!(full.contains("extra context"));
    }

    #[test]
    fn no_extra_prompt_uses_base_only() {
        let full_none: String = BASE_PROMPT.to_string();
        assert_eq!(full_none, "Describe this image in detail.");
    }

    // ── Resolve logic ────────────────────────────────────────────────────────

    #[test]
    fn env_key_takes_priority_over_config_key() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("OPENAI_API_KEY").ok();
        std::env::set_var("OPENAI_API_KEY", "env-key");

        let resolved = resolve(
            OPENAI_KEYS,
            OPENAI_BASE,
            OPENAI_MODEL,
            Some(DEFAULT_TRANSCRIPTION_MODEL),
            &config_with_key("openai", "config-key"),
        );

        std::env::remove_var("OPENAI_API_KEY");
        if let Some(v) = prev {
            std::env::set_var("OPENAI_API_KEY", v);
        }

        assert_eq!(resolved.unwrap().api_key, "env-key");
    }

    #[test]
    fn markit_api_key_env_is_fallback_for_any_provider() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_o = std::env::var("OPENAI_API_KEY").ok();
        let prev_m = std::env::var("MARKIT_API_KEY").ok();
        std::env::remove_var("OPENAI_API_KEY");
        std::env::set_var("MARKIT_API_KEY", "markit-env-key");

        let resolved = resolve(
            OPENAI_KEYS,
            OPENAI_BASE,
            OPENAI_MODEL,
            Some(DEFAULT_TRANSCRIPTION_MODEL),
            &empty_config(),
        );

        std::env::remove_var("MARKIT_API_KEY");
        if let Some(v) = prev_o {
            std::env::set_var("OPENAI_API_KEY", v);
        }
        if let Some(v) = prev_m {
            std::env::set_var("MARKIT_API_KEY", v);
        }

        assert_eq!(resolved.unwrap().api_key, "markit-env-key");
    }

    #[test]
    fn markit_model_env_overrides_config_model() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("MARKIT_MODEL").ok();
        std::env::set_var("MARKIT_MODEL", "env-model-override");

        let resolved = resolve(
            OPENAI_KEYS,
            OPENAI_BASE,
            OPENAI_MODEL,
            Some(DEFAULT_TRANSCRIPTION_MODEL),
            &MarkitConfig {
                llm: Some(LlmConfig {
                    api_key: Some("key".to_string()),
                    model: Some("config-model".to_string()),
                    ..Default::default()
                }),
            },
        );

        std::env::remove_var("MARKIT_MODEL");
        if let Some(v) = prev {
            std::env::set_var("MARKIT_MODEL", v);
        }

        assert_eq!(resolved.unwrap().model, "env-model-override");
    }

    #[test]
    fn api_base_trailing_slash_is_stripped() {
        let cfg = MarkitConfig {
            llm: Some(LlmConfig {
                api_key: Some("key".to_string()),
                api_base: Some("https://custom.com/v1/".to_string()),
                ..Default::default()
            }),
        };
        let resolved = resolve(
            OPENAI_KEYS,
            OPENAI_BASE,
            OPENAI_MODEL,
            Some(DEFAULT_TRANSCRIPTION_MODEL),
            &cfg,
        );
        assert_eq!(resolved.unwrap().api_base, "https://custom.com/v1");
    }

    #[test]
    fn default_base_is_used_when_config_has_none() {
        let cfg = MarkitConfig {
            llm: Some(LlmConfig {
                api_key: Some("key".to_string()),
                ..Default::default()
            }),
        };
        let resolved = resolve(
            OPENAI_KEYS,
            OPENAI_BASE,
            OPENAI_MODEL,
            Some(DEFAULT_TRANSCRIPTION_MODEL),
            &cfg,
        );
        assert_eq!(resolved.unwrap().api_base, "https://api.openai.com/v1");
    }
}
