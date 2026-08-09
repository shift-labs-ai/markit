//! Plugin system — installer, manifest, loader with bun subprocess bridge.
//!
//! JS plugins cannot run in-process in Rust. The loader uses a bun subprocess
//! bridge: each installed JS plugin's converters are wrapped in a BridgedConverter
//! that spawns bun with a small inline shim script to invoke accepts/convert.

pub mod bridge;
pub mod installer;
pub mod loader;
pub mod types;
