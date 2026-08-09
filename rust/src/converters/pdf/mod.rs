//! PDF conversion pipeline. Port of src/converters/pdf/.
//! TODO(phase-pdf): wired into the registry when extract.rs (mupdf) + index lands.
#![allow(dead_code)]
pub mod columns;
pub mod grid;
pub mod render;
pub mod types;
