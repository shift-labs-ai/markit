pub mod csv;
pub mod json;
pub mod plain_text;
pub mod xml;
pub mod yaml;

pub(crate) fn decode_text(input: &[u8]) -> String {
    String::from_utf8_lossy(input).into_owned()
}
