//! Plugin loader — mirrors src/plugins/loader.ts.
//!
//! Loads all installed plugins from .markit/plugins.json and returns their
//! converters as BridgedConverters. If bun is not on PATH, returns Ok(vec![])
//! with a stderr warning (graceful, matching TS behavior for broken plugins).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::bridge::{bun_available, discover_converters, BridgedConverter};
use super::installer::read_plugins_json;

/// Walk up from cwd to find .markit/ directory.
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

/// Find the entry point for a plugin directory.
/// Mirrors loadPluginFromPath() in loader.ts.
fn find_entry_point(abs_path: &Path) -> Option<PathBuf> {
    if abs_path.is_file() {
        return Some(abs_path.to_path_buf());
    }
    if !abs_path.is_dir() {
        return None;
    }

    let mut candidates = Vec::new();

    // Check package.json main field first
    let pkg_path = abs_path.join("package.json");
    if pkg_path.exists() {
        if let Ok(content) = fs::read_to_string(&pkg_path) {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(main) = pkg["main"].as_str() {
                    candidates.push(abs_path.join(main));
                }
            }
        }
    }

    candidates.extend([
        abs_path.join("src").join("index.ts"),
        abs_path.join("src").join("index.js"),
        abs_path.join("index.ts"),
        abs_path.join("index.js"),
    ]);

    candidates.into_iter().find(|c| c.exists())
}

/// Load all plugins from .markit/plugins.json and return their converters.
///
/// If bun is not on PATH, returns Ok(vec![]) with a stderr warning.
/// Individual plugin load failures are logged to stderr and skipped
/// (matching TS behavior of silently catching per-plugin errors).
pub fn load_all_plugins() -> Result<Vec<Box<dyn crate::types::Converter>>> {
    let config_dir = match find_config_dir() {
        Some(cd) => cd,
        None => return Ok(vec![]),
    };

    let plugins_file = config_dir.join("plugins.json");
    if !plugins_file.exists() {
        return Ok(vec![]);
    }

    let data = read_plugins_json();
    if data.plugins.is_empty() {
        return Ok(vec![]);
    }

    if !bun_available() {
        eprintln!(
            "Warning: bun not found on PATH — plugins cannot be loaded.              Install bun to use JS plugins."
        );
        return Ok(vec![]);
    }

    let mut converters: Vec<Box<dyn crate::types::Converter>> = Vec::new();

    for entry in &data.plugins {
        let plugin_path = &entry.path;
        let entry_point = match find_entry_point(Path::new(plugin_path)) {
            Some(ep) => ep,
            None => {
                eprintln!(
                    "Warning: no entry point found for plugin at {}",
                    plugin_path
                );
                continue;
            }
        };

        let ep_str = entry_point.to_string_lossy().to_string();
        match discover_converters(&ep_str) {
            Ok(metas) => {
                for meta in metas {
                    converters.push(Box::new(BridgedConverter::new(
                        ep_str.clone(),
                        meta.name,
                        meta.index,
                        meta.accepted_extensions,
                        meta.accepted_mimetypes,
                    )));
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: failed to load plugin at {}: {}",
                    plugin_path, e
                );
            }
        }
    }

    Ok(converters)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_config_dir_returns_empty() {
        // Run in a temp dir with no .markit
        let tmp = std::env::temp_dir().join("markit_test_loader_noconfig");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let result = load_all_plugins().unwrap();
        assert!(result.is_empty());

        std::env::set_current_dir(original).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn no_plugins_json_returns_empty() {
        let tmp = std::env::temp_dir().join("markit_test_loader_nojson");
        let _ = std::fs::remove_dir_all(&tmp);
        let markit_dir = tmp.join(".markit");
        std::fs::create_dir_all(&markit_dir).unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let result = load_all_plugins().unwrap();
        assert!(result.is_empty());

        std::env::set_current_dir(original).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_plugins_list_returns_empty() {
        let tmp = std::env::temp_dir().join("markit_test_loader_empty");
        let _ = std::fs::remove_dir_all(&tmp);
        let markit_dir = tmp.join(".markit");
        std::fs::create_dir_all(&markit_dir).unwrap();
        std::fs::write(
            markit_dir.join("plugins.json"),
            r#"{"plugins":[]}"#,
        )
        .unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let result = load_all_plugins().unwrap();
        assert!(result.is_empty());

        std::env::set_current_dir(original).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_entry_point_file() {
        let tmp = std::env::temp_dir().join("markit_test_entry_file");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("plugin.js");
        std::fs::write(&file, "module.exports = {}").unwrap();

        let result = find_entry_point(&file);
        assert_eq!(result, Some(file));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_entry_point_dir_with_index() {
        let tmp = std::env::temp_dir().join("markit_test_entry_dir");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let index = tmp.join("index.js");
        std::fs::write(&index, "module.exports = {}").unwrap();

        let result = find_entry_point(&tmp);
        assert_eq!(result, Some(index));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_entry_point_dir_with_src_index() {
        let tmp = std::env::temp_dir().join("markit_test_entry_src");
        let _ = std::fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let index = src.join("index.ts");
        std::fs::write(&index, "export default function() {}").unwrap();

        let result = find_entry_point(&tmp);
        assert_eq!(result, Some(index));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_entry_point_dir_with_package_json_main() {
        let tmp = std::env::temp_dir().join("markit_test_entry_pkg");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let main_file = tmp.join("dist").join("main.js");
        std::fs::create_dir_all(tmp.join("dist")).unwrap();
        std::fs::write(&main_file, "module.exports = {}").unwrap();
        std::fs::write(
            tmp.join("package.json"),
            r#"{"main": "dist/main.js"}"#,
        )
        .unwrap();

        let result = find_entry_point(&tmp);
        assert_eq!(result, Some(main_file));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_entry_point_nonexistent() {
        let result = find_entry_point(Path::new("/nonexistent/path"));
        assert!(result.is_none());
    }
}
