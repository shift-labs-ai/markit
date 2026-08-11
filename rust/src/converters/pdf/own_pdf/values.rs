//! Zero-copy PDF object values and dictionary lookup.

#[derive(Clone, Debug)]
pub enum Val<'a> {
    Null,
    Bool(bool),
    Num(f64),
    Name(&'a [u8]),
    Str(Vec<u8>),
    Array(Vec<Val<'a>>),
    Dict(Dict<'a>),
    /// Indirect reference (object number; generations are ignored).
    Ref(u32),
    /// Stream: dictionary + raw (still-encoded) bytes.
    Stream(Dict<'a>, &'a [u8]),
}

pub type Dict<'a> = Vec<(&'a [u8], Val<'a>)>;

pub fn dget<'a, 'b>(dict: &'b Dict<'a>, key: &[u8]) -> Option<&'b Val<'a>> {
    dict.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}

impl<'a> Val<'a> {
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Val::Num(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_name(&self) -> Option<&'a [u8]> {
        match self {
            Val::Name(n) => Some(n),
            _ => None,
        }
    }
}
