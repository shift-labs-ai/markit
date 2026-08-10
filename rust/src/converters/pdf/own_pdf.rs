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
    /// AES-256 stream decryption (V5 standard handler), when active.
    decrypt: Option<Decryptor>,
    /// Legacy (V1/V2/V4) decryption: per-object keys.
    legacy: Option<LegacyCrypt>,
    /// Decrypted stream bytes for legacy encryption, keyed by object
    /// number (entries are never evicted, so returned slices stay valid).
    legacy_cache: RefCell<FxHashMap<u32, Vec<u8>>>,
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
        let mut pdf = Pdf {
            data,
            xref: FxHashMap::default(),
            trailer: Vec::new(),
            objstm_cache: RefCell::new(FxHashMap::default()),
            decrypt: None,
            legacy: None,
            legacy_cache: RefCell::new(FxHashMap::default()),
        };
        let loaded = match find_startxref(data) {
            Ok(start) => pdf.load_xref_chain(start).is_ok() && !pdf.trailer.is_empty(),
            Err(_) => false,
        };
        if !loaded {
            pdf.repair_scan()?;
        }
        // Empty-password AES-256 documents decrypt transparently; anything
        // else errors here and the caller reports the failure.
        pdf.setup_decryption()?;
        Ok(pdf)
    }

    /// Damaged-file recovery: scavenge "N G obj" markers for offsets and
    /// find a trailer (or any /Root-bearing catalog) the hard way.
    fn repair_scan(&mut self) -> Result<()> {
        self.xref.clear();
        let data = self.data;
        let finder = memchr::memmem::Finder::new(b" obj");
        let mut at = 0usize;
        while let Some(rel) = finder.find(&data[at..]) {
            let hit = at + rel;
            // Walk back over "N G" digits/whitespace to the object number.
            let mut j = hit;
            let mut seen_gen = false;
            let mut seen_num = false;
            let mut num_end = 0usize;
            let mut num_start = 0usize;
            while j > 0 {
                let b = data[j - 1];
                if b.is_ascii_digit() {
                    if !seen_gen {
                        while j > 0 && data[j - 1].is_ascii_digit() {
                            j -= 1;
                        }
                        seen_gen = true;
                    } else {
                        num_end = j;
                        while j > 0 && data[j - 1].is_ascii_digit() {
                            j -= 1;
                        }
                        num_start = j;
                        seen_num = true;
                        break;
                    }
                } else if b == b' ' || b == b'\r' || b == b'\n' {
                    j -= 1;
                } else {
                    break;
                }
            }
            if seen_num && num_end > num_start {
                let num: u32 = std::str::from_utf8(&data[num_start..num_end])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                // Later definitions win in damaged files.
                self.xref.insert(num, XrefEntry::Offset(num_start));
            }
            at = hit + 4;
        }
        if self.xref.is_empty() {
            bail!("repair scan found no objects");
        }

        // Trailer: last "trailer" keyword, else hunt for a /Type /Catalog.
        if let Some(t) = memchr::memmem::rfind(data, b"trailer") {
            let mut lx = ObjLexer::new(data, t + 7);
            if let Ok(Val::Dict(d)) = lx.value() {
                self.trailer = d;
            }
        }
        if dget(&self.trailer, b"Root").is_none() {
            let nums: Vec<u32> = self.xref.keys().copied().collect();
            for num in nums {
                if let Ok(Val::Dict(d)) = self.object(num) {
                    if matches!(dget(&d, b"Type"), Some(Val::Name(b"Catalog"))) {
                        self.trailer = vec![(b"Root".as_slice(), Val::Ref(num))];
                        break;
                    }
                }
            }
        }
        if dget(&self.trailer, b"Root").is_none() {
            bail!("repair scan found no catalog");
        }
        Ok(())
    }

    /// Parse without rejecting encryption (decryption setup / probing).
    pub fn parse_allow_encrypted(data: &'a [u8]) -> Result<Pdf<'a>> {
        let start = find_startxref(data)?;
        let mut pdf = Pdf {
            data,
            xref: FxHashMap::default(),
            trailer: Vec::new(),
            objstm_cache: RefCell::new(FxHashMap::default()),
            decrypt: None,
            legacy: None,
            legacy_cache: RefCell::new(FxHashMap::default()),
        };
        pdf.load_xref_chain(start)?;
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

    /// All known object numbers (survey/debug tooling).
    pub fn object_numbers(&self) -> Vec<u32> {
        self.xref.keys().copied().collect()
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
                if self.legacy.is_some() {
                    if let Val::Stream(d, raw) = v {
                        let plain = self.legacy_decrypt(num, raw)?;
                        return Ok(Val::Stream(d, plain));
                    }
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
    // Decryption applies to the raw bytes, before any filter. Streams
    // reached before setup (the xref stream itself) are never encrypted.
    let decrypted;
    let raw: &[u8] = if pdf.decrypt.is_some() {
        decrypted = pdf.decrypt_stream(raw)?;
        &decrypted
    } else {
        raw
    };

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
        b"ASCIIHexDecode" | b"AHx" => {
            let mut out = Vec::with_capacity(data.len() / 2);
            let mut hi: Option<u8> = None;
            for &b in data {
                if b == b'>' {
                    break;
                }
                let v = match b {
                    b'0'..=b'9' => b - b'0',
                    b'a'..=b'f' => b - b'a' + 10,
                    b'A'..=b'F' => b - b'A' + 10,
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
            Ok(out)
        }
        b"ASCII85Decode" | b"A85" => ascii85(data),
        b"LZWDecode" | b"LZW" => lzw_decode(data),
        b"RunLengthDecode" | b"RL" => {
            let mut out = Vec::with_capacity(data.len() * 2);
            let mut i = 0usize;
            while i < data.len() {
                let l = data[i];
                i += 1;
                match l {
                    0..=127 => {
                        let n = l as usize + 1;
                        if i + n > data.len() {
                            break;
                        }
                        out.extend_from_slice(&data[i..i + n]);
                        i += n;
                    }
                    128 => break, // EOD
                    _ => {
                        if i >= data.len() {
                            break;
                        }
                        out.extend(std::iter::repeat_n(data[i], 257 - l as usize));
                        i += 1;
                    }
                }
            }
            Ok(out)
        }
        _ => bail!("unsupported filter {}", String::from_utf8_lossy(name)),
    }
}

/// LZW as PDF uses it (MSB-first codes, 9–12 bits, EarlyChange=1).
fn lzw_decode(data: &[u8]) -> Result<Vec<u8>> {
    const CLEAR: u16 = 256;
    const EOD: u16 = 257;

    let mut out = Vec::with_capacity(data.len() * 3);
    let mut dict: Vec<Vec<u8>> = (0..=257u16).map(|i| vec![i as u8]).collect();
    let mut code_len = 9usize;
    let mut prev: Option<u16> = None;

    let mut bitbuf = 0u32;
    let mut bits = 0usize;
    let mut i = 0usize;

    loop {
        while bits < code_len {
            if i >= data.len() {
                return Ok(out);
            }
            bitbuf = (bitbuf << 8) | data[i] as u32;
            bits += 8;
            i += 1;
        }
        let code = ((bitbuf >> (bits - code_len)) & ((1 << code_len) - 1)) as u16;
        bits -= code_len;

        match code {
            CLEAR => {
                dict.truncate(258);
                code_len = 9;
                prev = None;
            }
            EOD => return Ok(out),
            _ => {
                let entry: Vec<u8> = if (code as usize) < dict.len() {
                    dict[code as usize].clone()
                } else if let Some(p) = prev {
                    // KwKwK case
                    let mut e = dict[p as usize].clone();
                    e.push(dict[p as usize][0]);
                    e
                } else {
                    bail!("bad LZW stream");
                };
                out.extend_from_slice(&entry);
                if let Some(p) = prev {
                    let mut ne = dict[p as usize].clone();
                    ne.push(entry[0]);
                    dict.push(ne);
                }
                prev = Some(code);
                // EarlyChange=1: widen one code early.
                if dict.len() + 1 >= (1 << code_len) && code_len < 12 {
                    code_len += 1;
                }
            }
        }
    }
}

fn ascii85(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 4 / 5);
    let mut group = [0u8; 5];
    let mut n = 0usize;
    let mut i = 0usize;
    // optional <~ prefix
    if data.starts_with(b"<~") {
        i = 2;
    }
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            b'~' => break, // ~> EOD
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group[n] = b - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u32, |a, &d| a * 85 + d as u32);
                    out.extend_from_slice(&v.to_be_bytes());
                    n = 0;
                }
            }
            _ => {} // whitespace
        }
    }
    if n > 0 {
        // Partial group: pad with 'u' (84), emit n-1 bytes.
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u32, |a, &d| a * 85 + d as u32);
        out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
    }
    Ok(out)
}

/// Public shim for image extraction.
pub fn inflate_pub(data: &[u8]) -> Result<Vec<u8>> {
    inflate(data)
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

/// Debug helper: describe a document's /Encrypt dictionary.
pub fn probe_encrypt_dict(data: &[u8]) -> Result<String> {
    let pdf = Pdf::parse_allow_encrypted(data)?;
    let Some(enc) = dget(&pdf.trailer, b"Encrypt") else {
        return Ok("not encrypted".into());
    };
    let Val::Dict(enc) = pdf.resolve(enc)? else {
        bail!("bad Encrypt");
    };
    let mut out = String::new();
    for (k, v) in &enc {
        let vs = match pdf.resolve(v)? {
            Val::Name(n) => String::from_utf8_lossy(n).into_owned(),
            Val::Num(n) => n.to_string(),
            Val::Str(s) => format!("<{} bytes>", s.len()),
            Val::Dict(d) => {
                let keys: Vec<String> = d
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{}:{:?}",
                            String::from_utf8_lossy(k),
                            match v {
                                Val::Dict(inner) => inner
                                    .iter()
                                    .map(|(ik, iv)| format!(
                                        "{}={}",
                                        String::from_utf8_lossy(ik),
                                        match iv {
                                            Val::Name(n) => String::from_utf8_lossy(n).into_owned(),
                                            Val::Num(n) => n.to_string(),
                                            _ => "?".into(),
                                        }
                                    ))
                                    .collect::<Vec<_>>()
                                    .join(","),
                                _ => "?".to_string(),
                            }
                        )
                    })
                    .collect();
                keys.join(" ")
            }
            other => format!("{other:?}"),
        };
        out.push_str(&format!("{} = {vs}\n", String::from_utf8_lossy(k)));
    }
    Ok(out)
}

// ── Standard security handler, V5 (AES-256, revisions 5 and 6) ─────────────
//
// Empty-password documents only (the common "encrypted for permissions"
// case). Anything else — user passwords, RC4/AESV2 revisions — errors out
// to the MuPDF fallback.

pub(crate) struct Decryptor {
    key: [u8; 32],
}

impl<'a> Pdf<'a> {
    fn setup_decryption(&mut self) -> Result<()> {
        let Some(enc) = dget(&self.trailer, b"Encrypt") else {
            return Ok(());
        };
        let Val::Dict(enc) = self.resolve(enc)? else {
            bail!("bad Encrypt");
        };
        let g = |key: &[u8]| -> Result<Option<Val<'a>>> { self.dict_get(&enc, key) };

        if !matches!(g(b"Filter")?, Some(Val::Name(b"Standard"))) {
            bail!("non-standard security handler");
        }
        let v = g(b"V")?.and_then(|v| v.as_num()).unwrap_or(0.0) as i64;
        let r = g(b"R")?.and_then(|v| v.as_num()).unwrap_or(0.0) as i64;
        if matches!(v, 1 | 2 | 4) && matches!(r, 2..=4) {
            return self.setup_legacy(&enc, v, r);
        }
        if v != 5 || !(r == 5 || r == 6) {
            bail!("unsupported encryption V={v} R={r}");
        }
        // Stream crypt filter must be AESV3 (or Identity = nothing to do).
        match g(b"StmF")? {
            None | Some(Val::Name(b"Identity")) => return Ok(()),
            Some(Val::Name(b"StdCF")) => {}
            _ => bail!("unsupported StmF"),
        }
        if let Some(Val::Dict(cf)) = g(b"CF")? {
            if let Some(Val::Dict(stdcf)) = self.dict_get(&cf, b"StdCF")? {
                match self.dict_get(&stdcf, b"CFM")? {
                    Some(Val::Name(b"AESV3")) => {}
                    other => bail!("unsupported CFM {other:?}"),
                }
            }
        }

        let get_str = |k: &[u8]| -> Result<Vec<u8>> {
            match self.dict_get(&enc, k)? {
                Some(Val::Str(s)) => Ok(s),
                _ => bail!("missing {}", String::from_utf8_lossy(k)),
            }
        };
        let u = get_str(b"U")?;
        let ue = get_str(b"UE")?;
        let o = get_str(b"O")?;
        let oe = get_str(b"OE")?;
        if u.len() < 48 || o.len() < 48 || ue.len() < 32 || oe.len() < 32 {
            bail!("short U/O/UE/OE");
        }

        // Empty USER password (ISO 32000-2, 7.6.4.3.3/4).
        if hash_2b(b"", &u[32..40], b"", r) == u[0..32] {
            let ik = hash_2b(b"", &u[40..48], b"", r);
            let key = aes256_cbc_nopad_decrypt(&ik, &[0u8; 16], &ue[..32])?;
            self.decrypt = Some(Decryptor {
                key: key.try_into().map_err(|_| anyhow!("bad UE"))?,
            });
            return Ok(());
        }
        // Empty OWNER password (uses U as extra hash data).
        if hash_2b(b"", &o[32..40], &u[..48], r) == o[0..32] {
            let ik = hash_2b(b"", &o[40..48], &u[..48], r);
            let key = aes256_cbc_nopad_decrypt(&ik, &[0u8; 16], &oe[..32])?;
            self.decrypt = Some(Decryptor {
                key: key.try_into().map_err(|_| anyhow!("bad OE"))?,
            });
            return Ok(());
        }
        bail!("password required");
    }

    /// Public shim for image extraction.
    pub fn decrypt_stream_pub(&self, raw: &[u8]) -> Result<Vec<u8>> {
        self.decrypt_stream(raw)
    }

    /// Legacy (V<5) key schedule: Algorithm 2 with the empty user password.
    fn setup_legacy(&mut self, enc: &Dict<'a>, v: i64, r: i64) -> Result<()> {
        let get_str = |k: &[u8]| -> Result<Vec<u8>> {
            match self.dict_get(enc, k)? {
                Some(Val::Str(s)) => Ok(s),
                _ => bail!("missing {}", String::from_utf8_lossy(k)),
            }
        };
        let o = get_str(b"O")?;
        let u = get_str(b"U")?;
        let p = self
            .dict_get(enc, b"P")?
            .and_then(|x| x.as_num())
            .unwrap_or(0.0) as i64;
        let length_bits = self
            .dict_get(enc, b"Length")?
            .and_then(|x| x.as_num())
            .unwrap_or(40.0) as usize;
        let key_len = if v == 1 {
            5
        } else {
            (length_bits / 8).clamp(5, 16)
        };

        let mut aes = false;
        if v == 4 {
            match self.dict_get(enc, b"StmF")? {
                None | Some(Val::Name(b"Identity")) => return Ok(()),
                Some(Val::Name(b"StdCF")) => {}
                _ => bail!("unsupported StmF"),
            }
            if let Some(Val::Dict(cf)) = self.dict_get(enc, b"CF")? {
                if let Some(Val::Dict(stdcf)) = self.dict_get(&cf, b"StdCF")? {
                    match self.dict_get(&stdcf, b"CFM")? {
                        Some(Val::Name(b"AESV2")) => aes = true,
                        Some(Val::Name(b"V2")) => {}
                        other => bail!("unsupported CFM {other:?}"),
                    }
                }
            }
        }
        let encrypt_metadata = !matches!(
            self.dict_get(enc, b"EncryptMetadata")?,
            Some(Val::Bool(false))
        );

        // First file ID from the trailer.
        let id0: Vec<u8> = match dget(&self.trailer, b"ID").map(|x| self.resolve(x)) {
            Some(Ok(Val::Array(a))) => match a.first().map(|x| self.resolve(x)) {
                Some(Ok(Val::Str(s))) => s,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };

        // Algorithm 2 with the empty (padded) user password.
        let mut seed = Vec::with_capacity(128);
        seed.extend_from_slice(&PAD);
        seed.extend_from_slice(&o[..32.min(o.len())]);
        seed.extend_from_slice(&(p as i32).to_le_bytes());
        seed.extend_from_slice(&id0);
        if r >= 4 && !encrypt_metadata {
            seed.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        }
        let mut key = md5(&seed)[..key_len].to_vec();
        if r >= 3 {
            for _ in 0..50 {
                key = md5(&key)[..key_len].to_vec();
            }
        }

        // Validate against /U (Algorithm 6): tolerate producers that get
        // the padding wrong by comparing only the first 16 bytes for R>=3.
        let ok = if r == 2 {
            rc4(&key, &PAD) == u[..32.min(u.len())]
        } else {
            let mut h = Vec::with_capacity(64);
            h.extend_from_slice(&PAD);
            h.extend_from_slice(&id0);
            let mut x = md5(&h).to_vec();
            for i in 1..=19u8 {
                let k2: Vec<u8> = key.iter().map(|&b| b ^ i).collect();
                x = rc4(&k2, &x);
            }
            let x = rc4(&key, &x);
            u.len() >= 16 && x[..16] == u[..16]
        };
        if !ok {
            bail!("password required");
        }

        self.legacy = Some(LegacyCrypt { key, aes });
        Ok(())
    }

    /// Legacy per-object stream decryption with caching.
    pub(crate) fn legacy_decrypt(&self, num: u32, raw: &[u8]) -> Result<&'a [u8]> {
        let lc = self.legacy.as_ref().expect("legacy crypt");
        if !self.legacy_cache.borrow().contains_key(&num) {
            let plain = lc.decrypt_object(num, 0, raw)?;
            self.legacy_cache.borrow_mut().insert(num, plain);
        }
        let cache = self.legacy_cache.borrow();
        let v = cache.get(&num).unwrap();
        // Entries are never evicted; the Vec's heap allocation is stable.
        Ok(unsafe { std::slice::from_raw_parts(v.as_ptr(), v.len()) })
    }

    pub(crate) fn decrypt_stream(&self, raw: &[u8]) -> Result<Vec<u8>> {
        let Some(d) = &self.decrypt else {
            return Ok(raw.to_vec());
        };
        if raw.len() < 16 {
            bail!("encrypted stream too short");
        }
        let mut out = aes256_cbc_nopad_decrypt(&d.key, &raw[..16], &raw[16..])?;
        // PKCS#7 unpadding (tolerant: some producers pad wrong).
        if let Some(&pad) = out.last() {
            if pad >= 1 && pad as usize <= 16 && pad as usize <= out.len() {
                let n = out.len() - pad as usize;
                if out[n..].iter().all(|&b| b == pad) {
                    out.truncate(n);
                }
            }
        }
        Ok(out)
    }
}

/// Legacy standard security handler state (V1/V2 RC4, V4 RC4/AESV2).
pub(crate) struct LegacyCrypt {
    key: Vec<u8>,
    aes: bool,
}

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Tiny MD5 (RFC 1321) — enough for the legacy key schedule.
fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: [u32; 64] =
        std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32);
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);

    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while !msg.len().is_multiple_of(64) {
        if msg.len() % 64 == 56 {
            break;
        }
        msg.push(0);
    }
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let m: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ])
        });
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(k[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..].copy_from_slice(&d0.to_le_bytes());
    out
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|&b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            b ^ s[(s[i as usize].wrapping_add(s[j as usize])) as usize]
        })
        .collect()
}

impl LegacyCrypt {
    /// Per-object key (Algorithm 1) + decrypt.
    fn decrypt_object(&self, num: u32, gen: u16, data: &[u8]) -> Result<Vec<u8>> {
        let mut seed = self.key.clone();
        seed.extend_from_slice(&num.to_le_bytes()[..3]);
        seed.extend_from_slice(&gen.to_le_bytes()[..2]);
        if self.aes {
            seed.extend_from_slice(b"sAlT");
        }
        let h = md5(&seed);
        let klen = (self.key.len() + 5).min(16);
        let obj_key = &h[..klen];

        if self.aes {
            if data.len() < 16 || !data.len().is_multiple_of(16) {
                bail!("bad AESV2 stream");
            }
            use aes::cipher::{BlockDecryptMut, KeyIvInit};
            type Dec = cbc::Decryptor<aes::Aes128>;
            let mut buf = data[16..].to_vec();
            let mut dec = Dec::new(obj_key.into(), data[..16].into());
            for chunk in buf.chunks_exact_mut(16) {
                dec.decrypt_block_mut(chunk.into());
            }
            if let Some(&pad) = buf.last() {
                if pad >= 1 && pad as usize <= 16 && pad as usize <= buf.len() {
                    let n = buf.len() - pad as usize;
                    if buf[n..].iter().all(|&b| b == pad) {
                        buf.truncate(n);
                    }
                }
            }
            Ok(buf)
        } else {
            Ok(rc4(obj_key, data))
        }
    }
}

/// ISO 32000-2 Algorithm 2.B (revision 6 hardened hash; revision 5 is a
/// single SHA-256).
fn hash_2b(pw: &[u8], salt: &[u8], udata: &[u8], r: i64) -> [u8; 32] {
    use sha2::{Digest, Sha256, Sha384, Sha512};

    let mut k: Vec<u8> = {
        let mut h = Sha256::new();
        h.update(pw);
        h.update(salt);
        h.update(udata);
        h.finalize().to_vec()
    };
    if r == 5 {
        return k[..32].try_into().unwrap();
    }

    let mut round = 0usize;
    loop {
        // K1 = (pw ‖ K ‖ udata) × 64
        let unit_len = pw.len() + k.len() + udata.len();
        let mut k1 = Vec::with_capacity(unit_len * 64);
        for _ in 0..64 {
            k1.extend_from_slice(pw);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let e = aes128_cbc_nopad_encrypt(&k[..16], &k[16..32], &k1);
        let sum: u32 = e[..16].iter().map(|&b| b as u32).sum();
        k = match sum % 3 {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        round += 1;
        if round >= 64 && (*e.last().unwrap() as usize) <= round - 32 {
            break;
        }
    }
    k[..32].try_into().unwrap()
}

fn aes128_cbc_nopad_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit};
    type Enc = cbc::Encryptor<aes::Aes128>;
    let mut buf = data.to_vec();
    let mut enc = Enc::new(key.into(), iv.into());
    for chunk in buf.chunks_exact_mut(16) {
        enc.encrypt_block_mut(chunk.into());
    }
    buf
}

fn aes256_cbc_nopad_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    type Dec = cbc::Decryptor<aes::Aes256>;
    if !data.len().is_multiple_of(16) {
        bail!("ciphertext not block-aligned");
    }
    let mut buf = data.to_vec();
    let mut dec = Dec::new(key.into(), iv.into());
    for chunk in buf.chunks_exact_mut(16) {
        dec.decrypt_block_mut(chunk.into());
    }
    Ok(buf)
}
