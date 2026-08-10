//! Minimal lazy PDF object layer for the fast path.
//!
//! Parses only what text extraction needs, on demand: xref (classic
//! tables and xref streams, with /Prev chains), compressed object
//! streams (ObjStm), and individual objects as zero-copy values over the
//! input buffer. FlateDecode with PNG predictors is the only filter —
//! anything else (or encryption) errors out to the caller's fallback.

use anyhow::{anyhow, bail, Result};
use rustc_hash::FxHashMap;
use std::cell::RefCell;

// ── Values ──────────────────────────────────────────────────────────────────

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

// ── Document ────────────────────────────────────────────────────────────────

pub struct Pdf<'a> {
    data: &'a [u8],
    /// obj num → xref entry.
    xref: FxHashMap<u32, XrefEntry>,
    pub trailer: Dict<'a>,
    /// Decompressed object streams, keyed by their object number.
    objstm_cache: RefCell<FxHashMap<u32, ObjStm>>,
}

#[derive(Clone, Copy, Debug)]
enum XrefEntry {
    Offset(usize),
    InStream { stream_obj: u32, index: usize },
}

struct ObjStm {
    data: Vec<u8>,
    /// (obj num, offset into data after First).
    offsets: Vec<(u32, usize)>,
    first: usize,
}

impl<'a> Pdf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Pdf<'a>> {
        let start = find_startxref(data)?;
        let mut pdf = Pdf {
            data,
            xref: FxHashMap::default(),
            trailer: Vec::new(),
            objstm_cache: RefCell::new(FxHashMap::default()),
        };
        pdf.load_xref_chain(start)?;
        if dget(&pdf.trailer, b"Encrypt").is_some() {
            bail!("encrypted");
        }
        Ok(pdf)
    }

    fn load_xref_chain(&mut self, mut at: usize) -> Result<()> {
        // Follow /Prev; entries from newer sections win (first insert).
        for _ in 0..64 {
            let trailer = self.load_xref_section(at)?;
            if self.trailer.is_empty() {
                self.trailer = trailer.clone();
            }
            // XRefStm: hybrid files put stream entries alongside a table.
            if let Some(Val::Num(x)) = dget(&trailer, b"XRefStm") {
                let _ = self.load_xref_section(*x as usize);
            }
            match dget(&trailer, b"Prev") {
                Some(Val::Num(p)) => at = *p as usize,
                _ => return Ok(()),
            }
        }
        Ok(())
    }

    fn load_xref_section(&mut self, at: usize) -> Result<Dict<'a>> {
        let mut lx = ObjLexer::new(self.data, at);
        lx.skip_ws();
        if self.data[lx.pos..].starts_with(b"xref") {
            self.load_xref_table(at)
        } else {
            self.load_xref_stream(at)
        }
    }

    fn load_xref_table(&mut self, at: usize) -> Result<Dict<'a>> {
        let mut lx = ObjLexer::new(self.data, at);
        lx.skip_ws();
        lx.pos += 4; // "xref"
        loop {
            lx.skip_ws();
            if self.data[lx.pos..].starts_with(b"trailer") {
                lx.pos += 7;
                lx.skip_ws();
                return match lx.value()? {
                    Val::Dict(d) => Ok(d),
                    _ => bail!("bad trailer"),
                };
            }
            let first = lx.uint()? as u32;
            lx.skip_ws();
            let count = lx.uint()? as usize;
            lx.skip_ws();
            for i in 0..count {
                // Fixed 20-byte entries: 10 offset, 5 gen, 1 type.
                let entry = &self.data[lx.pos..lx.pos + 20.min(self.data.len() - lx.pos)];
                if entry.len() < 18 {
                    bail!("truncated xref");
                }
                let offset: usize = std::str::from_utf8(&entry[0..10])?.trim().parse()?;
                let kind = entry[17];
                let num = first + i as u32;
                if kind == b'n' {
                    self.xref.entry(num).or_insert(XrefEntry::Offset(offset));
                }
                lx.pos += 20;
                // tolerate 19-byte lines (single-byte EOL)
                if self.data.get(lx.pos.wrapping_sub(1)) == Some(&b'n')
                    || self.data.get(lx.pos.wrapping_sub(1)) == Some(&b'f')
                {
                    lx.pos -= 1;
                    lx.skip_ws();
                }
            }
        }
    }

    fn load_xref_stream(&mut self, at: usize) -> Result<Dict<'a>> {
        let mut lx = ObjLexer::new(self.data, at);
        let (_, val) = lx.indirect_object(self)?;
        let Val::Stream(dict, raw) = val else {
            bail!("xref stream expected");
        };
        let data = decode_stream(&dict, raw, self)?;

        let w: Vec<usize> = match dget(&dict, b"W") {
            Some(Val::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_num())
                .map(|v| v as usize)
                .collect(),
            _ => bail!("xref stream missing W"),
        };
        if w.len() < 3 {
            bail!("bad W");
        }
        let row = w[0] + w[1] + w[2];
        let size = dget(&dict, b"Size").and_then(|v| v.as_num()).unwrap_or(0.0) as u32;
        let index: Vec<u32> = match dget(&dict, b"Index") {
            Some(Val::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_num())
                .map(|v| v as u32)
                .collect(),
            _ => vec![0, size],
        };

        let mut pos = 0usize;
        for pair in index.chunks(2) {
            let [first, count] = pair else { break };
            for i in 0..*count {
                if pos + row > data.len() {
                    break;
                }
                let f = |o: usize, l: usize| -> u64 {
                    data[pos + o..pos + o + l]
                        .iter()
                        .fold(0u64, |acc, b| (acc << 8) | *b as u64)
                };
                let t = if w[0] == 0 { 1 } else { f(0, w[0]) };
                let b2 = f(w[0], w[1]);
                let b3 = f(w[0] + w[1], w[2]);
                let num = first + i;
                match t {
                    1 => {
                        self.xref
                            .entry(num)
                            .or_insert(XrefEntry::Offset(b2 as usize));
                    }
                    2 => {
                        self.xref.entry(num).or_insert(XrefEntry::InStream {
                            stream_obj: b2 as u32,
                            index: b3 as usize,
                        });
                    }
                    _ => {}
                }
                pos += row;
            }
        }
        Ok(dict)
    }

    /// Resolve an object by number, parsing it on demand.
    pub fn object(&self, num: u32) -> Result<Val<'a>> {
        match self.xref.get(&num) {
            Some(XrefEntry::Offset(at)) => {
                let mut lx = ObjLexer::new(self.data, *at);
                let (n, v) = lx.indirect_object(self)?;
                if n != num {
                    bail!("xref offset mismatch for {num}");
                }
                Ok(v)
            }
            Some(XrefEntry::InStream { stream_obj, index }) => {
                let (stream_obj, index) = (*stream_obj, *index);
                self.ensure_objstm(stream_obj)?;
                let cache = self.objstm_cache.borrow();
                let stm = cache.get(&stream_obj).ok_or_else(|| anyhow!("objstm"))?;
                let (onum, off) = *stm
                    .offsets
                    .get(index)
                    .ok_or_else(|| anyhow!("objstm index"))?;
                if onum != num {
                    bail!("objstm num mismatch");
                }
                // Parse out of the decompressed buffer. The buffer lives in
                // the cache for the document's lifetime; the borrow is safe
                // because entries are never evicted. Values borrowing from
                // it can't use 'a, so re-parse into owned-ish values via a
                // leaked-slice trick is avoided: we parse with a fresh
                // lexer over a stable slice obtained unsafely.
                let slice: &'a [u8] =
                    unsafe { std::slice::from_raw_parts(stm.data.as_ptr(), stm.data.len()) };
                let mut lx = ObjLexer::new(slice, stm.first + off);
                lx.value_with(self)
            }
            None => Ok(Val::Null),
        }
    }

    fn ensure_objstm(&self, num: u32) -> Result<()> {
        if self.objstm_cache.borrow().contains_key(&num) {
            return Ok(());
        }
        let Val::Stream(dict, raw) = self.object(num)? else {
            bail!("not an objstm");
        };
        let data = decode_stream(&dict, raw, self)?;
        let n = dget(&dict, b"N").and_then(|v| v.as_num()).unwrap_or(0.0) as usize;
        let first = dget(&dict, b"First")
            .and_then(|v| v.as_num())
            .unwrap_or(0.0) as usize;
        let mut offsets = Vec::with_capacity(n);
        {
            let mut lx = ObjLexer::new(&data, 0);
            for _ in 0..n {
                lx.skip_ws();
                let onum = lx.uint()? as u32;
                lx.skip_ws();
                let off = lx.uint()? as usize;
                offsets.push((onum, off));
            }
        }
        self.objstm_cache.borrow_mut().insert(
            num,
            ObjStm {
                data,
                offsets,
                first,
            },
        );
        Ok(())
    }

    /// Deep-resolve a value: follow Ref until a concrete value.
    pub fn resolve(&self, v: &Val<'a>) -> Result<Val<'a>> {
        let mut cur = v.clone();
        for _ in 0..32 {
            match cur {
                Val::Ref(n) => cur = self.object(n)?,
                other => return Ok(other),
            }
        }
        Ok(cur)
    }

    pub fn dict_get(&self, dict: &Dict<'a>, key: &[u8]) -> Result<Option<Val<'a>>> {
        match dget(dict, key) {
            Some(v) => Ok(Some(self.resolve(v)?)),
            None => Ok(None),
        }
    }
}

fn find_startxref(data: &[u8]) -> Result<usize> {
    let tail_start = data.len().saturating_sub(2048);
    let tail = &data[tail_start..];
    let at = memchr::memmem::rfind(tail, b"startxref").ok_or_else(|| anyhow!("no startxref"))?;
    let mut lx = ObjLexer::new(data, tail_start + at + 9);
    lx.skip_ws();
    Ok(lx.uint()? as usize)
}

// ── Stream decoding ─────────────────────────────────────────────────────────

pub fn decode_stream<'a>(dict: &Dict<'a>, raw: &[u8], pdf: &Pdf<'a>) -> Result<Vec<u8>> {
    let filter = pdf.dict_get(dict, b"Filter")?;
    let mut out = match filter {
        None => raw.to_vec(),
        Some(Val::Name(n)) => apply_filter(n, raw)?,
        Some(Val::Array(fs)) => {
            let mut cur = raw.to_vec();
            for f in fs {
                let Val::Name(n) = pdf.resolve(&f)? else {
                    bail!("bad filter entry");
                };
                cur = apply_filter(n, &cur)?;
            }
            cur
        }
        _ => bail!("bad Filter"),
    };

    // Predictors (xref streams, ObjStm): PNG predictors only.
    let parms = pdf.dict_get(dict, b"DecodeParms")?;
    if let Some(Val::Dict(p)) = parms {
        let predictor = pdf
            .dict_get(&p, b"Predictor")?
            .and_then(|v| v.as_num())
            .unwrap_or(1.0) as i64;
        if predictor >= 10 {
            let columns = pdf
                .dict_get(&p, b"Columns")?
                .and_then(|v| v.as_num())
                .unwrap_or(1.0) as usize;
            let colors = pdf
                .dict_get(&p, b"Colors")?
                .and_then(|v| v.as_num())
                .unwrap_or(1.0) as usize;
            out = png_unpredict(&out, columns * colors)?;
        } else if predictor != 1 {
            bail!("unsupported predictor");
        }
    }
    Ok(out)
}

fn apply_filter(name: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    match name {
        b"FlateDecode" | b"Fl" => inflate(data),
        _ => bail!("unsupported filter {}", String::from_utf8_lossy(name)),
    }
}

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::with_capacity(data.len() * 4);
    flate2::read::ZlibDecoder::new(data)
        .read_to_end(&mut out)
        .map_err(|e| anyhow!("inflate: {e}"))?;
    Ok(out)
}

fn png_unpredict(data: &[u8], row_len: usize) -> Result<Vec<u8>> {
    if row_len == 0 {
        bail!("bad predictor columns");
    }
    let stride = row_len + 1;
    let rows = data.len() / stride;
    let mut out = vec![0u8; rows * row_len];
    for r in 0..rows {
        let ftype = data[r * stride];
        let src = &data[r * stride + 1..r * stride + stride];
        for c in 0..row_len {
            let left = if c > 0 { out[r * row_len + c - 1] } else { 0 };
            let up = if r > 0 { out[(r - 1) * row_len + c] } else { 0 };
            let ul = if r > 0 && c > 0 {
                out[(r - 1) * row_len + c - 1]
            } else {
                0
            };
            let v = match ftype {
                0 => src[c],
                1 => src[c].wrapping_add(left),
                2 => src[c].wrapping_add(up),
                3 => src[c].wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => {
                    // Paeth
                    let p = left as i16 + up as i16 - ul as i16;
                    let (pa, pb, pc) = (
                        (p - left as i16).abs(),
                        (p - up as i16).abs(),
                        (p - ul as i16).abs(),
                    );
                    let pred = if pa <= pb && pa <= pc {
                        left
                    } else if pb <= pc {
                        up
                    } else {
                        ul
                    };
                    src[c].wrapping_add(pred)
                }
                _ => bail!("bad PNG predictor row"),
            };
            out[r * row_len + c] = v;
        }
    }
    Ok(out)
}

// ── Object lexer ────────────────────────────────────────────────────────────

pub struct ObjLexer<'a> {
    data: &'a [u8],
    pos: usize,
}

#[inline]
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\0')
}

#[inline]
fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
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

    fn uint(&mut self) -> Result<u64> {
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if start == self.pos {
            bail!("expected integer at {start}");
        }
        Ok(self.data[start..self.pos]
            .iter()
            .fold(0u64, |a, b| a * 10 + (b - b'0') as u64))
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
            let len = match pdf.dict_get(&dict, b"Length")? {
                Some(Val::Num(n)) => n as usize,
                _ => bail!("stream without Length"),
            };
            if self.pos + len > self.data.len() {
                bail!("stream length past EOF");
            }
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
                        let val = self.value()?;
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
                    arr.push(self.value()?);
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
        while self.pos < self.data.len()
            && !is_ws(self.data[self.pos])
            && !is_delim(self.data[self.pos])
        {
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
