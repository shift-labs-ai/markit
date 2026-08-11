//! Bounds-checked PDF object lexer: numbers, names, strings, arrays,
//! dictionaries, references, indirect objects, and stream boundaries.
//! Malformed input returns an error; it never indexes beyond the buffer.

use anyhow::{bail, Result};

use super::document::Pdf;
use super::values::{Dict, Val};

pub struct ObjLexer<'a> {
    pub(super) data: &'a [u8],
    pub(super) pos: usize,
}

/// 256-entry byte-class table: one L1-resident lookup classifies a
/// byte as whitespace and/or delimiter in a single load, replacing two
/// compare chains in every scan loop of both lexers.
const WS_BIT: u8 = 1;
const DELIM_BIT: u8 = 2;

pub(crate) static LEX_CLASS: [u8; 256] = {
    let mut t = [0u8; 256];
    let ws = b" \t\r\n\x0c\0";
    let delim = b"()<>[]{}/%";
    let mut i = 0;
    while i < ws.len() {
        t[ws[i] as usize] |= WS_BIT;
        i += 1;
    }
    let mut j = 0;
    while j < delim.len() {
        t[delim[j] as usize] |= DELIM_BIT;
        j += 1;
    }
    t
};

#[inline]
pub(crate) fn is_ws(b: u8) -> bool {
    LEX_CLASS[b as usize] & WS_BIT != 0
}

#[inline]
pub(crate) fn is_delim(b: u8) -> bool {
    LEX_CLASS[b as usize] & DELIM_BIT != 0
}

/// Neither whitespace nor delimiter: a regular token byte.
#[inline]
pub(crate) fn is_regular(b: u8) -> bool {
    LEX_CLASS[b as usize] == 0
}

impl<'a> ObjLexer<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        ObjLexer { data, pos }
    }

    pub fn skip_ws(&mut self) {
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while self.pos < self.data.len() && !matches!(self.data[self.pos], b'\n' | b'\r') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    pub(super) fn uint(&mut self) -> Result<u64> {
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if start == self.pos {
            bail!("expected integer at {start}");
        }
        self.data[start..self.pos].iter().try_fold(0u64, |n, b| {
            n.checked_mul(10)
                .and_then(|n| n.checked_add((b - b'0') as u64))
                .ok_or_else(|| anyhow::anyhow!("integer overflow at {start}"))
        })
    }

    /// Parse "N G obj <value> [stream…endstream] endobj".
    pub fn indirect_object(&mut self, pdf: &Pdf<'a>) -> Result<(u32, Val<'a>)> {
        self.skip_ws();
        let num = self.uint()? as u32;
        self.skip_ws();
        let _gen = self.uint()?;
        self.skip_ws();
        if !self.data[self.pos..].starts_with(b"obj") {
            bail!("expected obj at {}", self.pos);
        }
        self.pos += 3;
        let v = self.value_with(pdf)?;
        self.skip_ws();
        if self.data[self.pos..].starts_with(b"stream") {
            self.pos += 6;
            // EOL after "stream": CRLF or LF.
            if self.data.get(self.pos) == Some(&b'\r') {
                self.pos += 1;
            }
            if self.data.get(self.pos) == Some(&b'\n') {
                self.pos += 1;
            }
            let Val::Dict(dict) = v else {
                bail!("stream without dict")
            };
            // /Length may be indirect, wrong, or absent in damaged files;
            // trust it only when "endstream" actually follows the span.
            let len = match pdf.dict_get(&dict, b"Length") {
                Ok(Some(Val::Num(n))) => Some(n as usize),
                _ => None,
            };
            let valid = |l: usize| -> bool {
                let end = self.pos + l;
                if end > self.data.len() {
                    return false;
                }
                let tail = &self.data[end..(end + 20).min(self.data.len())];
                tail.iter()
                    .position(|&b| !matches!(b, b'\r' | b'\n' | b' ' | b'\t'))
                    .is_some_and(|i| tail[i..].starts_with(b"endstream"))
            };
            let len = match len {
                Some(l) if valid(l) => l,
                _ => {
                    // Recover: nearest following "endstream" delimiter.
                    let hay = &self.data[self.pos..];
                    let Some(at) = memchr::memmem::find(hay, b"endstream") else {
                        bail!("stream without endstream");
                    };
                    // Trim the EOL that precedes the keyword.
                    let mut l = at;
                    if l > 0 && hay[l - 1] == b'\n' {
                        l -= 1;
                    }
                    if l > 0 && hay[l - 1] == b'\r' {
                        l -= 1;
                    }
                    l
                }
            };
            let raw = &self.data[self.pos..self.pos + len];
            return Ok((num, Val::Stream(dict, raw)));
        }
        Ok((num, v))
    }

    /// Parse a value; needs the Pdf only to keep the signature uniform
    /// (references are returned unresolved).
    pub fn value_with(&mut self, pdf: &Pdf<'a>) -> Result<Val<'a>> {
        let _ = pdf;
        self.value()
    }

    pub fn value(&mut self) -> Result<Val<'a>> {
        self.value_depth(0)
    }

    fn value_depth(&mut self, depth: usize) -> Result<Val<'a>> {
        const MAX_VALUE_DEPTH: usize = 64;
        if depth >= MAX_VALUE_DEPTH {
            bail!("PDF object nesting exceeds {MAX_VALUE_DEPTH}");
        }
        self.skip_ws();
        let Some(&b) = self.data.get(self.pos) else {
            bail!("eof in value");
        };
        match b {
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    let mut dict: Dict<'a> = Vec::with_capacity(8);
                    loop {
                        self.skip_ws();
                        if self.data[self.pos..].starts_with(b">>") {
                            self.pos += 2;
                            return Ok(Val::Dict(dict));
                        }
                        if self.data.get(self.pos) != Some(&b'/') {
                            bail!("dict key expected at {}", self.pos);
                        }
                        let key = self.name_body();
                        let val = self.value_depth(depth + 1)?;
                        dict.push((key, val));
                    }
                } else {
                    // hex string
                    self.pos += 1;
                    let mut out = Vec::new();
                    let mut hi: Option<u8> = None;
                    while self.pos < self.data.len() {
                        let c = self.data[self.pos];
                        self.pos += 1;
                        if c == b'>' {
                            break;
                        }
                        let v = match c {
                            b'0'..=b'9' => c - b'0',
                            b'a'..=b'f' => c - b'a' + 10,
                            b'A'..=b'F' => c - b'A' + 10,
                            _ => continue,
                        };
                        match hi.take() {
                            Some(h) => out.push((h << 4) | v),
                            None => hi = Some(v),
                        }
                    }
                    if let Some(h) = hi {
                        out.push(h << 4);
                    }
                    Ok(Val::Str(out))
                }
            }
            b'/' => Ok(Val::Name(self.name_body())),
            b'[' => {
                self.pos += 1;
                let mut arr = Vec::with_capacity(8);
                loop {
                    self.skip_ws();
                    if self.data.get(self.pos) == Some(&b']') {
                        self.pos += 1;
                        return Ok(Val::Array(arr));
                    }
                    arr.push(self.value_depth(depth + 1)?);
                }
            }
            b'(' => {
                self.pos += 1;
                let mut out = Vec::new();
                let mut depth = 1usize;
                while self.pos < self.data.len() {
                    let c = self.data[self.pos];
                    self.pos += 1;
                    match c {
                        b'\\' => {
                            let Some(&e) = self.data.get(self.pos) else {
                                break;
                            };
                            self.pos += 1;
                            match e {
                                b'n' => out.push(b'\n'),
                                b'r' => out.push(b'\r'),
                                b't' => out.push(b'\t'),
                                b'b' => out.push(0x08),
                                b'f' => out.push(0x0c),
                                b'0'..=b'7' => {
                                    let mut v = (e - b'0') as u32;
                                    for _ in 0..2 {
                                        match self.data.get(self.pos) {
                                            Some(&d @ b'0'..=b'7') => {
                                                v = v * 8 + (d - b'0') as u32;
                                                self.pos += 1;
                                            }
                                            _ => break,
                                        }
                                    }
                                    out.push(v as u8);
                                }
                                b'\r' => {
                                    if self.data.get(self.pos) == Some(&b'\n') {
                                        self.pos += 1;
                                    }
                                }
                                b'\n' => {}
                                other => out.push(other),
                            }
                        }
                        b'(' => {
                            depth += 1;
                            out.push(c);
                        }
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            out.push(c);
                        }
                        _ => out.push(c),
                    }
                }
                Ok(Val::Str(out))
            }
            b'0'..=b'9' | b'+' | b'-' | b'.' => {
                // Number — or an indirect reference "N G R".
                let save = self.pos;
                let n1 = self.number();
                if b != b'+' && b != b'-' && b != b'.' && n1.fract() == 0.0 && n1 >= 0.0 {
                    let save2 = self.pos;
                    self.skip_ws();
                    let g_start = self.pos;
                    let mut ok = false;
                    while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() {
                        self.pos += 1;
                        ok = true;
                    }
                    if ok {
                        self.skip_ws();
                        if self.data.get(self.pos) == Some(&b'R')
                            && self
                                .data
                                .get(self.pos + 1)
                                .is_none_or(|c| is_ws(*c) || is_delim(*c))
                        {
                            self.pos += 1;
                            return Ok(Val::Ref(n1 as u32));
                        }
                    }
                    let _ = g_start;
                    self.pos = save2;
                }
                let _ = save;
                Ok(Val::Num(n1))
            }
            b't' if self.data[self.pos..].starts_with(b"true") => {
                self.pos += 4;
                Ok(Val::Bool(true))
            }
            b'f' if self.data[self.pos..].starts_with(b"false") => {
                self.pos += 5;
                Ok(Val::Bool(false))
            }
            b'n' if self.data[self.pos..].starts_with(b"null") => {
                self.pos += 4;
                Ok(Val::Null)
            }
            _ => bail!("unexpected byte {b:#x} at {}", self.pos),
        }
    }

    fn name_body(&mut self) -> &'a [u8] {
        self.pos += 1; // '/'
        let start = self.pos;
        while self.pos < self.data.len() && is_regular(self.data[self.pos]) {
            self.pos += 1;
        }
        // Note: '#'-escaped names are returned raw; the escape form is
        // rare in the keys the text path reads.
        &self.data[start..self.pos]
    }

    fn number(&mut self) -> f64 {
        let start = self.pos;
        if matches!(self.data[self.pos], b'+' | b'-') {
            self.pos += 1;
        }
        while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.data.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        let s = &self.data[start..self.pos];
        crate::converters::pdf::content_lex::fast_float_pub(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excessive_object_nesting_is_rejected() {
        let mut bytes = vec![b'['; 65];
        bytes.extend(std::iter::repeat_n(b']', 65));
        let mut lexer = ObjLexer::new(&bytes, 0);
        assert!(lexer.value().is_err(), "unbounded nesting was accepted");
    }

    #[test]
    fn oversized_integer_is_rejected_without_wrapping() {
        let mut lexer = ObjLexer::new(b"18446744073709551616", 0);
        assert!(lexer.uint().is_err());
    }
}
