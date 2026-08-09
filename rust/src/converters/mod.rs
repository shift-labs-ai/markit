pub mod audio;
pub mod csv;
pub mod docx;
pub mod epub;
pub mod github;
pub mod html;
pub mod image;
pub mod ipynb;
pub mod iwork;
pub mod json;
pub mod pdf;
pub mod plain_text;
pub mod pptx;
pub mod rss;
pub mod wikipedia;
pub mod xlsx;
pub mod xml;
pub mod yaml;
pub mod zip;

pub(crate) fn decode_text(input: &[u8]) -> String {
    String::from_utf8_lossy(input).into_owned()
}
