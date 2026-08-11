//! Lazy, zero-copy PDF object layer.
//!
//! Public surface is intentionally small; implementation lives behind
//! focused modules for values, documents, xrefs, lexical parsing,
//! stream filters, and security handlers.

mod crypto;
mod document;
mod filters;
mod lexer;
mod probe;
mod values;
mod xref;

pub use document::Pdf;
pub use filters::{decode_stream, inflate_pub};
pub use lexer::ObjLexer;
pub(crate) use lexer::{is_delim, is_ws};
pub use probe::probe_encrypt_dict;
pub use values::{dget, Dict, Val};
