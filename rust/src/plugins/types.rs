//! Plugin types — mirrors src/plugins/types.ts.

use serde::{Deserialize, Serialize};

/// Installed plugin record, stored in plugins.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub source: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The plugins.json manifest.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginsManifest {
    pub plugins: Vec<InstalledPlugin>,
}

/// Parsed plugin source descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginSource {
    pub kind: PluginSourceKind,
    pub name: String,
    pub ref_: Option<String>,
    pub url: Option<String>,
    pub path: Option<String>,
    pub subpath: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PluginSourceKind {
    Npm,
    Git,
    Local,
}
