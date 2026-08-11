//! Cross-reference parsing: classic tables, xref streams, hybrid
//! XRefStm files, and incremental /Prev chains.

use anyhow::{anyhow, bail, Result};

use super::document::Pdf;
use super::filters::decode_stream;
use super::lexer::ObjLexer;
use super::values::{dget, Dict, Val};

#[derive(Clone, Copy, Debug)]
pub(super) enum XrefEntry {
    Offset(usize),
    InStream { stream_obj: u32, index: usize },
}

impl<'a> Pdf<'a> {
    pub(super) fn load_xref_chain(&mut self, mut at: usize) -> Result<()> {
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

    pub(super) fn load_xref_section(&mut self, at: usize) -> Result<Dict<'a>> {
        if at >= self.data.len() {
            bail!("xref offset {at} past EOF");
        }
        let mut lx = ObjLexer::new(self.data, at);
        lx.skip_ws();
        if lx.pos >= self.data.len() {
            bail!("xref offset {at} points to EOF");
        }
        if self.data[lx.pos..].starts_with(b"xref") {
            self.load_xref_table(at)
        } else {
            self.load_xref_stream(at)
        }
    }

    pub(super) fn load_xref_table(&mut self, at: usize) -> Result<Dict<'a>> {
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
                if lx.pos >= self.data.len() {
                    bail!("truncated xref entry");
                }
                let available = self.data.len() - lx.pos;
                let entry = &self.data[lx.pos..lx.pos + 20.min(available)];
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

    pub(super) fn load_xref_stream(&mut self, at: usize) -> Result<Dict<'a>> {
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
}

pub(super) fn find_startxref(data: &[u8]) -> Result<usize> {
    let tail_start = data.len().saturating_sub(2048);
    let tail = &data[tail_start..];
    let at = memchr::memmem::rfind(tail, b"startxref").ok_or_else(|| anyhow!("no startxref"))?;
    let mut lx = ObjLexer::new(data, tail_start + at + 9);
    lx.skip_ws();
    Ok(lx.uint()? as usize)
}
