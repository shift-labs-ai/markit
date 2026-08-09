//! Bun subprocess bridge for JS plugins.
//!
//! Since JS plugins cannot run in-process in Rust, each installed JS plugin's
//! converters are wrapped in a BridgedConverter that spawns bun with a small
//! inline shim script to invoke accepts/convert.
//!
//! ## How it works
//!
//! At load time, for each plugin path we run a bun discovery shim that loads
//! the plugin module, invokes the plugin function (or reads the plugin object),
//! and outputs JSON metadata about its converters (name + accepted
//! extensions/mimetypes gathered by probing accepts()).
//!
//! At convert time, each BridgedConverter spawns bun with a convert shim that
//! reads base64 input from stdin and writes ConversionResult JSON to stdout.
//!
//! ## Divergences from TS
//!
//! - **Plugin providers (api.ts registerProvider)**: Not bridged. Providers
//!   require deep integration (API keys, HTTP clients, streaming) that makes
//!   subprocess bridging impractical. Converter bridging is the priority.
//! - **accepts() resolution**: Done at load time via a discovery shim that
//!   probes accepts() with common extensions/mimetypes. The TS version calls
//!   accepts() per-file at runtime; we cache the results since spawning bun
//!   per-file for accepts() would be too slow.

use std::process::Command;

use anyhow::{anyhow, Result};

use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

/// Check if bun is available on PATH.
pub fn bun_available() -> bool {
    Command::new("bun")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Metadata about a single converter discovered from a JS plugin.
#[derive(Debug, Clone)]
pub struct ConverterMeta {
    pub name: String,
    pub index: usize,
    pub accepted_extensions: Vec<String>,
    pub accepted_mimetypes: Vec<String>,
}

/// Generate the bun -e discovery shim script for a plugin at the given path.
pub fn discovery_shim(plugin_path: &str) -> String {
    format!(
        r#"
const mod = await import(Bun.pathToFileURL("{}").href);
const exported = mod.default || mod;

let converters = [];
if (typeof exported === 'function') {{
    const api = {{
        setName() {{}},
        setVersion() {{}},
        registerConverter(c, fmt) {{
            converters.push({{ converter: c, format: fmt }});
        }},
        registerProvider() {{}},
    }};
    exported(api);
}} else if (exported && exported.converters) {{
    converters = exported.converters.map(c => ({{ converter: c }}));
}}

const exts = ['.pdf','.docx','.pptx','.xlsx','.html','.htm','.epub','.ipynb',
    '.rss','.atom','.xml','.svg','.csv','.tsv','.json','.yaml','.yml',
    '.md','.txt','.js','.ts','.py','.rs','.go','.java','.c','.cpp',
    '.png','.jpg','.jpeg','.gif','.webp','.mp3','.wav','.ogg','.flac',
    '.m4a','.aac','.zip','.numbers','.pages','.key','.latex','.tex'];
const mimes = ['application/pdf','application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    'text/html','text/csv','text/plain','application/json','text/xml',
    'image/png','image/jpeg','audio/mpeg','application/zip'];

const results = converters.map((entry, i) => {{
    const c = entry.converter;
    const acceptedExts = [];
    const acceptedMimes = [];
    for (const ext of exts) {{
        try {{ if (c.accepts({{ extension: ext }})) acceptedExts.push(ext); }} catch {{}}
    }}
    for (const mime of mimes) {{
        try {{ if (c.accepts({{ mimetype: mime }})) acceptedMimes.push(mime); }} catch {{}}
    }}
    return {{
        name: c.name || 'plugin-converter-' + i,
        index: i,
        acceptedExtensions: acceptedExts,
        acceptedMimetypes: acceptedMimes,
    }};
}});

console.log(JSON.stringify(results));
"#, plugin_path)
}

/// Generate the bun -e convert shim script.
/// Input is passed as base64 on stdin, output is JSON on stdout.
pub fn convert_shim(plugin_path: &str, converter_index: usize) -> String {
    format!(
        r#"
import {{ Buffer }} from 'node:buffer';

const mod = await import(Bun.pathToFileURL("{}").href);
const exported = mod.default || mod;

let converters = [];
if (typeof exported === 'function') {{
    const api = {{
        setName() {{}},
        setVersion() {{}},
        registerConverter(c) {{ converters.push(c); }},
        registerProvider() {{}},
    }};
    exported(api);
}} else if (exported && exported.converters) {{
    converters = exported.converters;
}}

const converter = converters[{}];
if (!converter) {{
    console.error('Converter index {} not found');
    process.exit(1);
}}

// Read base64 input from stdin
const chunks = [];
for await (const chunk of Bun.stdin.stream()) {{
    chunks.push(chunk);
}}
const input = Buffer.concat(chunks);

// First line is JSON metadata, rest is base64 buffer
const newline = input.indexOf(10);
const metaLine = input.subarray(0, newline).toString('utf-8');
const b64data = input.subarray(newline + 1).toString('utf-8');
const meta = JSON.parse(metaLine);
const buffer = Buffer.from(b64data, 'base64');

const result = await converter.convert(buffer, meta.streamInfo, meta.options || {{}});
console.log(JSON.stringify({{
    markdown: typeof result === 'string' ? result : result.markdown,
    title: typeof result === 'object' ? result.title : undefined,
}}));
"#, plugin_path, converter_index, converter_index)
}

/// A converter that bridges to a JS plugin via bun subprocess.
pub struct BridgedConverter {
    pub plugin_path: String,
    pub converter_name: String,
    pub converter_index: usize,
    pub accepted_extensions: Vec<String>,
    pub accepted_mimetypes: Vec<String>,
    /// Leaked name for the 'static lifetime requirement of Converter::name().
    leaked_name: &'static str,
}

impl BridgedConverter {
    pub fn new(
        plugin_path: String,
        converter_name: String,
        converter_index: usize,
        accepted_extensions: Vec<String>,
        accepted_mimetypes: Vec<String>,
    ) -> Self {
        let leaked = Box::leak(format!("plugin:{}", converter_name).into_boxed_str());
        Self {
            plugin_path,
            converter_name,
            converter_index,
            accepted_extensions,
            accepted_mimetypes,
            leaked_name: leaked,
        }
    }
}

impl Converter for BridgedConverter {
    fn name(&self) -> &'static str {
        self.leaked_name
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ref ext) = info.extension {
            let ext_dot = if ext.starts_with('.') {
                ext.to_lowercase()
            } else {
                format!(".{}", ext.to_lowercase())
            };
            if self
                .accepted_extensions
                .iter()
                .any(|e| e.to_lowercase() == ext_dot)
            {
                return true;
            }
        }
        if let Some(ref mime) = info.mimetype {
            let mime_lower = mime.to_lowercase();
            if self
                .accepted_mimetypes
                .iter()
                .any(|m| m.to_lowercase() == mime_lower)
            {
                return true;
            }
        }
        false
    }

    fn convert(
        &self,
        input: &[u8],
        info: &StreamInfo,
        _options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        use base64::Engine;

        let shim = convert_shim(&self.plugin_path, self.converter_index);
        let b64 = base64::engine::general_purpose::STANDARD.encode(input);

        // Build metadata JSON
        let meta = serde_json::json!({
            "streamInfo": {
                "mimetype": info.mimetype,
                "extension": info.extension,
                "charset": info.charset,
                "filename": info.filename,
                "localPath": info.local_path,
                "url": info.url,
            },
            "options": {},
        });

        let stdin_data = format!("{}
{}", meta, b64);

        let mut child = Command::new("bun")
            .args(["-e", &shim])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("Failed to run bun: {e}"))?;

        {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(stdin_data.as_bytes())?;
            }
        }
        // Drop stdin to signal EOF
        child.stdin.take();

        let output = child
            .wait_with_output()
            .map_err(|e| anyhow!("Failed to wait for bun: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "Plugin converter '{}' failed: {}",
                self.converter_name,
                stderr.trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let result: serde_json::Value = serde_json::from_str(stdout.trim())
            .map_err(|e| anyhow!("Invalid JSON from plugin converter: {e}"))?;

        Ok(ConversionResult {
            markdown: result["markdown"].as_str().unwrap_or("").to_string(),
            title: result["title"].as_str().map(String::from),
        })
    }
}

/// Discover converters from a plugin path by running the discovery shim.
pub fn discover_converters(plugin_path: &str) -> Result<Vec<ConverterMeta>> {
    let shim = discovery_shim(plugin_path);

    let output = Command::new("bun")
        .args(["-e", &shim])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| anyhow!("Failed to run bun: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Plugin discovery failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let metas: Vec<serde_json::Value> = serde_json::from_str(stdout.trim())
        .map_err(|e| anyhow!("Invalid JSON from plugin discovery: {e}"))?;

    Ok(metas
        .into_iter()
        .map(|m| ConverterMeta {
            name: m["name"].as_str().unwrap_or("unknown").to_string(),
            index: m["index"].as_u64().unwrap_or(0) as usize,
            accepted_extensions: m["acceptedExtensions"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            accepted_mimetypes: m["acceptedMimetypes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_shim_contains_plugin_path() {
        let shim = discovery_shim("/path/to/plugin.js");
        assert!(shim.contains("/path/to/plugin.js"));
        assert!(shim.contains("registerConverter"));
        assert!(shim.contains("JSON.stringify"));
    }

    #[test]
    fn convert_shim_contains_converter_index() {
        let shim = convert_shim("/path/to/plugin.js", 2);
        assert!(shim.contains("/path/to/plugin.js"));
        assert!(shim.contains("converters[2]"));
        assert!(shim.contains("base64"));
    }

    #[test]
    fn bridged_converter_accepts_by_extension() {
        let bc = BridgedConverter::new(
            "/test".to_string(),
            "test".to_string(),
            0,
            vec![".latex".to_string(), ".tex".to_string()],
            vec![],
        );

        assert!(bc.accepts(&StreamInfo {
            extension: Some(".latex".to_string()),
            ..Default::default()
        }));
        // Without dot prefix
        assert!(bc.accepts(&StreamInfo {
            extension: Some("tex".to_string()),
            ..Default::default()
        }));
        assert!(!bc.accepts(&StreamInfo {
            extension: Some(".pdf".to_string()),
            ..Default::default()
        }));
    }

    #[test]
    fn bridged_converter_accepts_by_mimetype() {
        let bc = BridgedConverter::new(
            "/test".to_string(),
            "test".to_string(),
            0,
            vec![],
            vec!["application/x-latex".to_string()],
        );

        assert!(bc.accepts(&StreamInfo {
            mimetype: Some("application/x-latex".to_string()),
            ..Default::default()
        }));
        assert!(!bc.accepts(&StreamInfo {
            mimetype: Some("text/plain".to_string()),
            ..Default::default()
        }));
    }

    #[test]
    fn bridged_converter_rejects_empty_info() {
        let bc = BridgedConverter::new(
            "/test".to_string(),
            "test".to_string(),
            0,
            vec![".tex".to_string()],
            vec![],
        );

        assert!(!bc.accepts(&StreamInfo::default()));
    }

    #[test]
    fn bun_available_returns_bool() {
        // Just verify it doesn't panic — actual result depends on environment
        let _ = bun_available();
    }
}
