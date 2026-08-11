//! Lazy PDF document: open/repair, on-demand object resolution,
//! object-stream caching, references, and dictionary access. Xref
//! parsing, lexical parsing, filters, and security handlers are separate
//! sibling modules behind this type.

use anyhow::{anyhow, bail, Result};
use rustc_hash::FxHashMap;
use std::cell::RefCell;

use super::crypto::{Decryptor, LegacyCrypt};
use super::filters::decode_stream;
use super::lexer::ObjLexer;
use super::values::{dget, Dict, Val};
use super::xref::{find_startxref, XrefEntry};

pub struct Pdf<'a> {
    pub(super) data: &'a [u8],
    /// obj num → xref entry.
    pub(super) xref: FxHashMap<u32, XrefEntry>,
    pub trailer: Dict<'a>,
    /// Decompressed object streams, keyed by their object number.
    pub(super) objstm_cache: RefCell<FxHashMap<u32, ObjStm>>,
    /// AES-256 stream decryption (V5 standard handler), when active.
    pub(super) decrypt: Option<Decryptor>,
    /// Legacy (V1/V2/V4) decryption: per-object keys.
    pub(super) legacy: Option<LegacyCrypt>,
    /// Decrypted stream bytes for legacy encryption, keyed by object
    /// number (entries are never evicted, so returned slices stay valid).
    pub(super) legacy_cache: RefCell<FxHashMap<u32, Vec<u8>>>,
}

pub(super) struct ObjStm {
    pub(super) data: Vec<u8>,
    /// (obj num, offset into data after First).
    pub(super) offsets: Vec<(u32, usize)>,
    pub(super) first: usize,
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
    pub(super) fn repair_scan(&mut self) -> Result<()> {
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
}

impl<'a> Pdf<'a> {
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
                // SAFETY: ObjStm entries are inserted once into
                // objstm_cache and never removed or replaced; the Vec's
                // allocation therefore remains stable for the lifetime of
                // Pdf. Every value returned from this slice remains tied to
                // that same Pdf, so extending this stable allocation's
                // borrow to 'a cannot outlive its owner.
                let slice: &'a [u8] =
                    unsafe { std::slice::from_raw_parts(stm.data.as_ptr(), stm.data.len()) };
                let mut lx = ObjLexer::new(slice, stm.first + off);
                lx.value_with(self)
            }
            None => Ok(Val::Null),
        }
    }

    pub(super) fn ensure_objstm(&self, num: u32) -> Result<()> {
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
        bail!("reference chain too deep or cyclic")
    }

    pub fn dict_get(&self, dict: &Dict<'a>, key: &[u8]) -> Result<Option<Val<'a>>> {
        match dget(dict, key) {
            Some(v) => Ok(Some(self.resolve(v)?)),
            None => Ok(None),
        }
    }
}
