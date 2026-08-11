//! Cross-reference parsing: classic tables, xref streams, hybrid
//! XRefStm files, and incremental /Prev chains.

use anyhow::{anyhow, bail, Result};

use super::document::Pdf;
use super::filters::decode_stream;
use super::lexer::ObjLexer;
use super::values::{dget, Dict, Val};

#[derive(Clone, Copy, Debug)]
pub(super) enum XrefEntry {
    Offset { at: usize, generation: u16 },
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
                let generation: u16 = std::str::from_utf8(&entry[11..16])?.trim().parse()?;
                let kind = entry[17];
                let num = first
                    .checked_add(i as u32)
                    .ok_or_else(|| anyhow!("xref object number overflow"))?;
                if kind == b'n' {
                    self.xref.entry(num).or_insert(XrefEntry::Offset {
                        at: offset,
                        generation,
                    });
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
        if w.len() < 3 || w[..3].iter().any(|&width| width > 8) {
            bail!("bad W");
        }
        let row = w[0]
            .checked_add(w[1])
            .and_then(|n| n.checked_add(w[2]))
            .ok_or_else(|| anyhow!("xref row width overflow"))?;
        if row == 0 {
            bail!("zero-width xref rows");
        }
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
                let Some(row_end) = pos.checked_add(row) else {
                    bail!("xref stream position overflow");
                };
                if row_end > data.len() {
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
                let num = first
                    .checked_add(i)
                    .ok_or_else(|| anyhow!("xref object number overflow"))?;
                match t {
                    1 => {
                        let generation = u16::try_from(b3)
                            .map_err(|_| anyhow!("xref generation exceeds u16"))?;
                        self.xref.entry(num).or_insert(XrefEntry::Offset {
                            at: b2 as usize,
                            generation,
                        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use elsa::FrozenMap;
    use rustc_hash::FxHashMap;
    use std::cell::RefCell;

    fn blank_pdf(data: &[u8]) -> Pdf<'_> {
        Pdf {
            data,
            xref: FxHashMap::default(),
            trailer: Vec::new(),
            objstm_cache: FrozenMap::new(),
            objstm_in_progress: RefCell::new(Default::default()),
            decrypt: RefCell::new(None),
            legacy: RefCell::new(None),
            legacy_cache: FrozenMap::new(),
        }
    }

    #[test]
    fn zero_width_xref_rows_are_rejected() {
        let data = b"1 0 obj << /Type /XRef /W [0 0 0] /Size 1 /Length 0 >> stream

endstream
endobj";
        let mut pdf = blank_pdf(data);
        assert!(pdf.load_xref_stream(0).is_err(), "zero-width rows accepted");
    }

    #[test]
    fn xref_fields_wider_than_u64_are_rejected() {
        let data = b"1 0 obj << /Type /XRef /W [9 0 0] /Size 1 /Length 9 >> stream
123456789
endstream
endobj";
        let mut pdf = blank_pdf(data);
        assert!(
            pdf.load_xref_stream(0).is_err(),
            "oversized W field accepted"
        );
    }

    #[test]
    fn classic_xref_preserves_object_generation() {
        let data = b"xref
5 1
0000000000 00007 n \ntrailer << /Size 6 >>";
        let mut pdf = blank_pdf(data);
        pdf.load_xref_table(0).unwrap();
        assert!(matches!(
            pdf.xref.get(&5),
            Some(XrefEntry::Offset { generation: 7, .. })
        ));
    }

    #[test]
    fn classic_xref_object_number_overflow_is_rejected() {
        let data = b"xref
4294967295 2
0000000000 00000 n \n0000000000 00000 n \ntrailer << /Size 0 >>";
        let mut pdf = blank_pdf(data);
        assert!(pdf.load_xref_table(0).is_err());
    }
}
