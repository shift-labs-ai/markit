//! Plugin installer — mirrors src/plugins/installer.ts.
//!
//! Installs from npm:pkg / git:url / local path into .markit/plugins.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};

use super::types::{InstalledPlugin, PluginSource, PluginSourceKind, PluginsManifest};

const PLUGINS_FILE: &str = "plugins.json";

/// Parse a plugin source string into a structured descriptor.
/// Mirrors parsePluginSource() in installer.ts.
pub fn parse_plugin_source(source: &str) -> PluginSource {
    // npm:package@version
    if let Some(rest) = source.strip_prefix("npm:") {
        let (name, ref_) = if rest.starts_with('@') {
            // Scoped package: @scope/pkg possibly @version
            let last_at = rest.rfind('@').unwrap();
            let first_at = rest.find('@').unwrap();
            if last_at > 0 && last_at != first_at {
                (rest[..last_at].to_string(), Some(rest[last_at + 1..].to_string()))
            } else {
                (rest.to_string(), None)
            }
        } else {
            match rest.find('@') {
                Some(idx) if idx > 0 => {
                    (rest[..idx].to_string(), Some(rest[idx + 1..].to_string()))
                }
                _ => (rest.to_string(), None),
            }
        };
        return PluginSource {
            kind: PluginSourceKind::Npm,
            name,
            ref_,
            url: None,
            path: None,
            subpath: None,
        };
    }

    // git:url or https:// or http:// or ssh://
    if source.starts_with("git:")
        || source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("ssh://")
    {
        let raw = source.strip_prefix("git:").unwrap_or(source);

        // Extract subpath after #
        let (raw, subpath) = match raw.find('#') {
            Some(idx) if idx > 0 => (&raw[..idx], Some(raw[idx + 1..].to_string())),
            _ => (raw, None),
        };

        // Extract ref after last @, but only if no / follows the @
        let (raw, ref_) = {
            let r = raw.to_string();
            match r.rfind('@') {
                Some(idx) if idx > 0 && !r[idx..].contains('/') => {
                    (r[..idx].to_string(), Some(r[idx + 1..].to_string()))
                }
                _ => (r, None),
            }
        };

        // Ensure proper URL scheme
        let mut url = raw;
        if !url.starts_with("http://")
            && !url.starts_with("https://")
            && !url.starts_with("ssh://")
        {
            url = format!("https://{url}");
        }
        if !url.ends_with(".git") {
            url.push_str(".git");
        }

        let name = if let Some(ref sp) = subpath {
            Path::new(sp)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| sp.clone())
        } else {
            let stem = url.strip_suffix(".git").unwrap_or(&url);
            Path::new(stem)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| "plugin".to_string())
        };

        return PluginSource {
            kind: PluginSourceKind::Git,
            name,
            ref_,
            url: Some(url),
            path: None,
            subpath,
        };
    }

    // Local path
    let abs_path = fs::canonicalize(source)
        .unwrap_or_else(|_| PathBuf::from(source).canonicalize().unwrap_or_else(|_| PathBuf::from(source)));
    let abs_str = abs_path.to_string_lossy().to_string();
    let name = Path::new(&abs_str)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "plugin".to_string());

    PluginSource {
        kind: PluginSourceKind::Local,
        name,
        ref_: None,
        url: None,
        path: Some(abs_str),
        subpath: None,
    }
}

/// Walk up from `start` to find a .markit/ directory.
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

fn get_plugins_dir() -> PathBuf {
    let config_dir = find_config_dir();
    let dir = match config_dir {
        Some(cd) => cd.join("plugins"),
        None => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            cwd.join(".markit").join("plugins")
        }
    };
    fs::create_dir_all(&dir).ok();
    dir
}

fn get_plugins_json_path() -> PathBuf {
    let config_dir = find_config_dir();
    match config_dir {
        Some(cd) => cd.join(PLUGINS_FILE),
        None => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            cwd.join(".markit").join(PLUGINS_FILE)
        }
    }
}

pub fn read_plugins_json() -> PluginsManifest {
    let path = get_plugins_json_path();
    if !path.exists() {
        return PluginsManifest::default();
    }
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => PluginsManifest::default(),
    }
}

fn write_plugins_json(data: &PluginsManifest) -> Result<()> {
    let path = get_plugins_json_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(data)?;
    fs::write(&path, format!("{json}\n"))?;
    Ok(())
}

/// Install a plugin from a source string.
/// Returns (install_path, name).
pub fn install_plugin(source: &str) -> Result<(String, String)> {
    let parsed = parse_plugin_source(source);
    let plugins_dir = get_plugins_dir();
    let install_path: String;

    match parsed.kind {
        PluginSourceKind::Npm => {
            let npm_dir = plugins_dir.join("npm");
            fs::create_dir_all(&npm_dir)?;
            let spec = match &parsed.ref_ {
                Some(r) => format!("{}@{}", parsed.name, r),
                None => parsed.name.clone(),
            };
            let status = Command::new("npm")
                .args(["install", &spec])
                .current_dir(&npm_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .status()
                .map_err(|e| anyhow!("Failed to run npm: {e}"))?;
            if !status.success() {
                return Err(anyhow!("npm install failed with exit code: {}", status));
            }
            install_path = npm_dir
                .join("node_modules")
                .join(&parsed.name)
                .to_string_lossy()
                .to_string();
        }
        PluginSourceKind::Git => {
            let url_str = parsed.url.as_deref().ok_or_else(|| anyhow!("No URL for git source"))?;
            // Parse hostname and path from URL without the url crate.
            // URL is always scheme://host/path.git at this point.
            let after_scheme = url_str
                .strip_prefix("https://")
                .or_else(|| url_str.strip_prefix("http://"))
                .or_else(|| url_str.strip_prefix("ssh://"))
                .ok_or_else(|| anyhow!("Invalid URL: {url_str}"))?;
            let (hostname, url_path) = after_scheme
                .split_once('/')
                .unwrap_or((after_scheme, ""));
            let url_path = url_path.strip_suffix(".git").unwrap_or(url_path);
            let git_dir = plugins_dir.join("git").join(hostname).join(url_path);

            if git_dir.exists() {
                let status = Command::new("git")
                    .arg("pull")
                    .current_dir(&git_dir)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .status()
                    .map_err(|e| anyhow!("Failed to run git: {e}"))?;
                if !status.success() {
                    return Err(anyhow!("git pull failed"));
                }
            } else {
                if let Some(parent) = git_dir.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut args = vec!["clone"];
                let ref_val;
                if let Some(ref r) = parsed.ref_ {
                    ref_val = r.clone();
                    args.push("--branch");
                    args.push(&ref_val);
                }
                args.push(url_str);
                let dir_str = git_dir.to_string_lossy().to_string();
                args.push(&dir_str);
                let status = Command::new("git")
                    .args(&args)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .status()
                    .map_err(|e| anyhow!("Failed to run git: {e}"))?;
                if !status.success() {
                    return Err(anyhow!("git clone failed"));
                }
            }

            // npm install if package.json exists
            if git_dir.join("package.json").exists() {
                Command::new("npm")
                    .arg("install")
                    .current_dir(&git_dir)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .status()
                    .ok();
            }

            install_path = match &parsed.subpath {
                Some(sp) => git_dir.join(sp).to_string_lossy().to_string(),
                None => git_dir.to_string_lossy().to_string(),
            };
        }
        PluginSourceKind::Local => {
            let p = parsed
                .path
                .as_deref()
                .ok_or_else(|| anyhow!("No path for local source"))?;
            if !Path::new(p).exists() {
                return Err(anyhow!("Path does not exist: {p}"));
            }
            install_path = p.to_string();
        }
    }

    // Update plugins.json
    let mut data = read_plugins_json();
    let entry = InstalledPlugin {
        source: source.to_string(),
        path: install_path.clone(),
        name: Some(parsed.name.clone()),
    };

    if let Some(existing) = data.plugins.iter_mut().find(|p| p.source == source) {
        *existing = entry;
    } else {
        data.plugins.push(entry);
    }
    write_plugins_json(&data)?;

    Ok((install_path, parsed.name))
}

/// Remove a plugin by name.
pub fn remove_plugin(name: &str) -> Result<bool> {
    let mut data = read_plugins_json();
    let idx = data
        .plugins
        .iter()
        .position(|p| p.name.as_deref() == Some(name) || p.source.contains(name));

    let Some(idx) = idx else {
        return Ok(false);
    };

    let plugin = &data.plugins[idx];
    let parsed = parse_plugin_source(&plugin.source);

    // Don't delete local plugin files
    if parsed.kind != PluginSourceKind::Local && Path::new(&plugin.path).exists() {
        fs::remove_dir_all(&plugin.path).ok();
    }

    data.plugins.remove(idx);
    write_plugins_json(&data)?;
    Ok(true)
}

/// List installed plugins with parsed metadata.
pub fn list_installed() -> Vec<ListedPlugin> {
    let data = read_plugins_json();
    data.plugins
        .iter()
        .map(|p| {
            let parsed = parse_plugin_source(&p.source);
            let kind_str = match parsed.kind {
                PluginSourceKind::Npm => "npm",
                PluginSourceKind::Git => "git",
                PluginSourceKind::Local => "local",
            };
            ListedPlugin {
                name: p.name.clone().unwrap_or(parsed.name),
                kind: kind_str.to_string(),
                source: p.source.clone(),
                path: p.path.clone(),
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct ListedPlugin {
    pub name: String,
    pub kind: String,
    pub source: String,
    pub path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_npm_simple() {
        let s = parse_plugin_source("npm:markit-latex");
        assert_eq!(s.kind, PluginSourceKind::Npm);
        assert_eq!(s.name, "markit-latex");
        assert_eq!(s.ref_, None);
    }

    #[test]
    fn parse_npm_with_version() {
        let s = parse_plugin_source("npm:markit-latex@1.2.3");
        assert_eq!(s.kind, PluginSourceKind::Npm);
        assert_eq!(s.name, "markit-latex");
        assert_eq!(s.ref_.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn parse_npm_scoped() {
        let s = parse_plugin_source("npm:@acme/markit-plugin");
        assert_eq!(s.kind, PluginSourceKind::Npm);
        assert_eq!(s.name, "@acme/markit-plugin");
        assert_eq!(s.ref_, None);
    }

    #[test]
    fn parse_npm_scoped_with_version() {
        let s = parse_plugin_source("npm:@acme/markit-plugin@2.0.0");
        assert_eq!(s.kind, PluginSourceKind::Npm);
        assert_eq!(s.name, "@acme/markit-plugin");
        assert_eq!(s.ref_.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn parse_git_https() {
        let s = parse_plugin_source("https://github.com/user/repo");
        assert_eq!(s.kind, PluginSourceKind::Git);
        assert_eq!(s.name, "repo");
        assert_eq!(s.url.as_deref(), Some("https://github.com/user/repo.git"));
        assert_eq!(s.ref_, None);
    }

    #[test]
    fn parse_git_prefix() {
        let s = parse_plugin_source("git:github.com/user/repo");
        assert_eq!(s.kind, PluginSourceKind::Git);
        assert_eq!(s.name, "repo");
        assert_eq!(s.url.as_deref(), Some("https://github.com/user/repo.git"));
    }

    #[test]
    fn parse_git_with_subpath() {
        let s = parse_plugin_source("https://github.com/user/mono#packages/plugin");
        assert_eq!(s.kind, PluginSourceKind::Git);
        assert_eq!(s.name, "plugin");
        assert_eq!(s.subpath.as_deref(), Some("packages/plugin"));
    }

    #[test]
    fn parse_local_path() {
        let s = parse_plugin_source("/tmp/my-plugin.ts");
        assert_eq!(s.kind, PluginSourceKind::Local);
        assert_eq!(s.name, "my-plugin");
        assert_eq!(s.path.as_deref(), Some("/tmp/my-plugin.ts"));
    }

    #[test]
    fn parse_local_directory() {
        let s = parse_plugin_source("/tmp/my-plugin");
        assert_eq!(s.kind, PluginSourceKind::Local);
        assert_eq!(s.name, "my-plugin");
    }

    #[test]
    fn manifest_roundtrip() {
        let manifest = PluginsManifest {
            plugins: vec![
                InstalledPlugin {
                    source: "npm:test-plugin".to_string(),
                    path: "/some/path".to_string(),
                    name: Some("test-plugin".to_string()),
                },
            ],
        };
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let parsed: PluginsManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.plugins.len(), 1);
        assert_eq!(parsed.plugins[0].source, "npm:test-plugin");
        assert_eq!(parsed.plugins[0].name.as_deref(), Some("test-plugin"));
    }

    #[test]
    fn local_install_into_temp_dir() {
        // Create a temp dir with a .markit structure and a fake plugin file.
        // Use a unique name with thread id to avoid collisions.
        let tid = format!("{:?}", std::thread::current().id());
        let tmp = std::env::temp_dir().join(format!("markit_test_plugin_install_{}", tid.replace(|c: char| !c.is_alphanumeric(), "_")));
        let _ = fs::remove_dir_all(&tmp);
        let markit_dir = tmp.join(".markit");
        fs::create_dir_all(&markit_dir).unwrap();

        // Create a fake plugin file
        let plugin_file = tmp.join("my-plugin.js");
        fs::write(&plugin_file, "module.exports = function(api) {}").unwrap();

        // Change to the temp dir so find_config_dir finds .markit
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let source = plugin_file.to_string_lossy().to_string();
        let result = install_plugin(&source);

        // Verify install succeeded
        assert!(result.is_ok(), "install failed: {:?}", result.err());
        let (path, name) = result.unwrap();
        assert_eq!(name, "my-plugin");
        assert!(path.contains("my-plugin"));

        // Verify manifest was written — read directly from known path
        // to avoid cwd race conditions with parallel tests.
        let manifest_path = markit_dir.join("plugins.json");
        assert!(manifest_path.exists(), "plugins.json should exist");
        let content = fs::read_to_string(&manifest_path).unwrap();
        let manifest: PluginsManifest = serde_json::from_str(&content).unwrap();
        assert_eq!(manifest.plugins.len(), 1);
        assert_eq!(manifest.plugins[0].name.as_deref(), Some("my-plugin"));

        // Remove it
        let removed = remove_plugin("my-plugin").unwrap();
        assert!(removed);
        let content = fs::read_to_string(&manifest_path).unwrap();
        let manifest: PluginsManifest = serde_json::from_str(&content).unwrap();
        assert_eq!(manifest.plugins.len(), 0);

        // Restore
        std::env::set_current_dir(original_dir).unwrap();
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn local_install_nonexistent_path_fails() {
        let result = install_plugin("/nonexistent/path/plugin.js");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Path does not exist"));
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let tmp = std::env::temp_dir().join("markit_test_remove_noexist");
        let _ = fs::remove_dir_all(&tmp);
        let markit_dir = tmp.join(".markit");
        fs::create_dir_all(&markit_dir).unwrap();

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let removed = remove_plugin("nonexistent").unwrap();
        assert!(!removed);

        std::env::set_current_dir(original_dir).unwrap();
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn list_empty() {
        let tmp = std::env::temp_dir().join("markit_test_list_empty");
        let _ = fs::remove_dir_all(&tmp);
        let markit_dir = tmp.join(".markit");
        fs::create_dir_all(&markit_dir).unwrap();

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let list = list_installed();
        assert!(list.is_empty());

        std::env::set_current_dir(original_dir).unwrap();
        let _ = fs::remove_dir_all(&tmp);
    }
}
