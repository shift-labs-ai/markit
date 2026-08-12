//! Minimal sfnt (TrueType) parsing for unicode recovery.
//!
//! When a simple TrueType font has no ToUnicode CMap and no usable
//! /Encoding, the character codes are font-specific. The embedded font
//! program still knows what its glyphs mean: the cmap table maps
//! unicode->glyph (invertible), the symbol cmap maps code->glyph, and
//! the post table names glyphs outright. This is the same recovery
//! chain used by mature PDF engines.

fn u16be(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?]))
}
fn u32be(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *d.get(at)?,
        *d.get(at + 1)?,
        *d.get(at + 2)?,
        *d.get(at + 3)?,
    ]))
}

/// Locate a table in the sfnt directory.
fn find_table<'a>(font: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let num_tables = u16be(font, 4)? as usize;
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        if font.get(rec..rec + 4)? == tag {
            let off = u32be(font, rec + 8)? as usize;
            let len = u32be(font, rec + 12)? as usize;
            return font.get(off..(off + len).min(font.len()));
        }
    }
    None
}

/// Iterate a cmap subtable's (code, gid) pairs into a callback.
fn walk_subtable(sub: &[u8], mut emit: impl FnMut(u32, u16)) -> Option<()> {
    match u16be(sub, 0)? {
        0 => {
            // Byte encoding table: 256 direct entries.
            for code in 0..256usize {
                let gid = *sub.get(6 + code)? as u16;
                if gid != 0 {
                    emit(code as u32, gid);
                }
            }
        }
        4 => {
            let segcount = (u16be(sub, 6)? / 2) as usize;
            let ends = 14;
            let starts = ends + segcount * 2 + 2;
            let deltas = starts + segcount * 2;
            let ranges = deltas + segcount * 2;
            for s in 0..segcount {
                let end = u16be(sub, ends + s * 2)?;
                let start = u16be(sub, starts + s * 2)?;
                let delta = u16be(sub, deltas + s * 2)?;
                let range_off = u16be(sub, ranges + s * 2)?;
                if start == 0xFFFF {
                    continue;
                }
                for code in start..=end {
                    let gid = if range_off == 0 {
                        code.wrapping_add(delta)
                    } else {
                        let idx = ranges + s * 2 + range_off as usize + (code - start) as usize * 2;
                        let g = u16be(sub, idx)?;
                        if g == 0 {
                            continue;
                        }
                        g.wrapping_add(delta)
                    };
                    if gid != 0 {
                        emit(code as u32, gid);
                    }
                }
            }
        }
        6 => {
            let first = u16be(sub, 6)? as u32;
            let count = u16be(sub, 8)? as usize;
            for i in 0..count {
                let gid = u16be(sub, 10 + i * 2)?;
                if gid != 0 {
                    emit(first + i as u32, gid);
                }
            }
        }
        12 => {
            let ngroups = u32be(sub, 12)? as usize;
            const MAX_CMAP_MAPPINGS: usize = 1_000_000;
            let mut mappings = 0usize;
            for g in 0..ngroups {
                let rec = 16usize.checked_add(g.checked_mul(12)?)?;
                let start = u32be(sub, rec)?;
                let end = u32be(sub, rec + 4)?;
                let count = end.checked_sub(start)?.checked_add(1)? as usize;
                mappings = mappings.checked_add(count)?;
                if mappings > MAX_CMAP_MAPPINGS {
                    return None;
                }
                let start_gid = u32be(sub, rec + 8)?;
                for code in start..=end {
                    let gid = u16::try_from(start_gid.checked_add(code - start)?).ok()?;
                    emit(code, gid);
                }
            }
        }
        _ => {}
    }
    Some(())
}

/// Standard Macintosh glyph names order (post format 2 indexes 0..258).
/// Only the name->unicode step matters, so map indexes straight to chars
/// for the ASCII block and the common extras; 0 = unmapped.
fn mac_glyph_char(idx: u16) -> Option<char> {
    // 0 .notdef, 1 .null, 2 nonmarkingreturn, 3.. follow StandardMac order.
    const MAC: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_\u{60}abcdefghijklmnopqrstuvwxyz{|}~";
    let i = idx.checked_sub(3)? as usize;
    MAC.chars().nth(i)
}

/// Glyph advance widths in glyph-space /1000 units, indexed by glyph
/// id: head.unitsPerEm + hhea.numberOfHMetrics + hmtx. Glyphs past
/// numberOfHMetrics repeat the last advance (monospace tail), per spec.
pub fn gid_advances(font: &[u8]) -> Option<Vec<f64>> {
    let font = if font.get(..4) == Some(b"ttcf") {
        let off = u32be(font, 12)? as usize;
        font.get(off..)?
    } else {
        font
    };
    if !matches!(
        font.get(..4),
        Some([0, 1, 0, 0]) | Some(b"true") | Some(b"OTTO")
    ) {
        return None;
    }
    let head = find_table(font, b"head")?;
    let upem = u16be(head, 18)? as f64;
    if upem <= 0.0 {
        return None;
    }
    let hhea = find_table(font, b"hhea")?;
    let num_metrics = u16be(hhea, 34)? as usize;
    let maxp = find_table(font, b"maxp")?;
    let num_glyphs = (u16be(maxp, 4)? as usize).min(65_536);
    if num_metrics == 0 || num_glyphs == 0 {
        return None;
    }
    let hmtx = find_table(font, b"hmtx")?;
    let mut out = Vec::with_capacity(num_glyphs);
    let mut last = 0f64;
    for gid in 0..num_glyphs {
        if gid < num_metrics {
            last = u16be(hmtx, gid * 4)? as f64 / upem * 1000.0;
        }
        out.push(last);
    }
    Some(out)
}

/// Invert the font's unicode cmap: glyph id -> unicode. The recovery
/// chain for composite (CIDFontType2) fonts, where the PDF code is a
/// CID that maps to a glyph id via CIDToGIDMap.
pub fn gid_to_unicode(font: &[u8]) -> Option<rustc_hash::FxHashMap<u16, u32>> {
    let font = if font.get(..4) == Some(b"ttcf") {
        let off = u32be(font, 12)? as usize;
        font.get(off..)?
    } else {
        font
    };
    if !matches!(
        font.get(..4),
        Some([0, 1, 0, 0]) | Some(b"true") | Some(b"OTTO")
    ) {
        return None;
    }
    let cmap = find_table(font, b"cmap")?;
    let n = u16be(cmap, 2)? as usize;
    let mut uni: Option<&[u8]> = None;
    for i in 0..n {
        let rec = 4 + i * 8;
        let plat = u16be(cmap, rec)?;
        let enc = u16be(cmap, rec + 2)?;
        let off = u32be(cmap, rec + 4)? as usize;
        if matches!((plat, enc), (3, 1) | (3, 10) | (0, _)) {
            uni = Some(cmap.get(off..)?);
        }
    }
    let mut map = rustc_hash::FxHashMap::default();
    walk_subtable(uni?, |code, gid| {
        map.entry(gid).or_insert(code);
    })?;
    (!map.is_empty()).then_some(map)
}

/// Recover a code->unicode map (256 simple-font codes) from a TrueType
/// font program. Chain: symbol cmap for code->gid, unicode cmap inverted
/// for gid->unicode, post table names as the fallback.
pub fn code_to_unicode(font: &[u8]) -> Option<[u32; 256]> {
    // TTC header: use the first font.
    let font = if font.get(..4) == Some(b"ttcf") {
        let off = u32be(font, 12)? as usize;
        font.get(off..)?
    } else {
        font
    };
    if !matches!(
        font.get(..4),
        Some([0, 1, 0, 0]) | Some(b"true") | Some(b"OTTO")
    ) {
        return None;
    }
    let cmap = find_table(font, b"cmap")?;
    let n = u16be(cmap, 2)? as usize;
    let mut sym: Option<&[u8]> = None; // (3,0)
    let mut uni: Option<&[u8]> = None; // (3,1) / (0,x)
    let mut mac: Option<&[u8]> = None; // (1,0)
    for i in 0..n {
        let rec = 4 + i * 8;
        let plat = u16be(cmap, rec)?;
        let enc = u16be(cmap, rec + 2)?;
        let off = u32be(cmap, rec + 4)? as usize;
        let sub = cmap.get(off..)?;
        match (plat, enc) {
            (3, 0) => sym = Some(sub),
            (3, 1) | (3, 10) | (0, _) => uni = Some(sub),
            (1, 0) => mac = Some(sub),
            _ => {}
        }
    }

    let mut out = [0u32; 256];
    let mut mapped = false;

    if let (Some(s), Some(u)) = (sym, uni) {
        // gid -> unicode from the unicode table…
        let mut gid_uni: rustc_hash::FxHashMap<u16, u32> = rustc_hash::FxHashMap::default();
        walk_subtable(u, |code, gid| {
            gid_uni.entry(gid).or_insert(code);
        })?;
        // …then codes through the symbol table (F0xx convention or bare).
        walk_subtable(s, |code, gid| {
            let simple = if (0xF000..=0xF0FF).contains(&code) {
                code - 0xF000
            } else {
                code
            };
            if simple < 256 && out[simple as usize] == 0 {
                if let Some(&u) = gid_uni.get(&gid) {
                    out[simple as usize] = u;
                    mapped = true;
                }
            }
        })?;
    }

    if !mapped {
        if let Some(u) = uni {
            // Unicode cmap only: codes usually ARE unicode for the low range.
            walk_subtable(u, |code, _| {
                if code < 256 && out[code as usize] == 0 {
                    out[code as usize] = code;
                    mapped = true;
                }
            })?;
        }
    }

    if !mapped {
        // post format 2: glyph names. code->gid via (3,0)/(1,0) cmap.
        let src = sym.or(mac)?;
        let post = find_table(font, b"post")?;
        if u32be(post, 0)? == 0x0002_0000 {
            let numg = u16be(post, 32)? as usize;
            let mut code_gid = [0u16; 256];
            walk_subtable(src, |code, gid| {
                let c = if (0xF000..=0xF0FF).contains(&code) {
                    code - 0xF000
                } else {
                    code
                };
                if c < 256 {
                    code_gid[c as usize] = gid;
                }
            })?;
            // Collect custom names (index >= 258).
            let mut names: Vec<&[u8]> = Vec::new();
            let mut at = 34 + numg * 2;
            while at < post.len() {
                let len = post[at] as usize;
                names.push(post.get(at + 1..at + 1 + len)?);
                at += 1 + len;
            }
            for c in 0..256usize {
                let gid = code_gid[c] as usize;
                if gid == 0 || gid >= numg {
                    continue;
                }
                let idx = u16be(post, 34 + gid * 2)?;
                let ch = if idx < 258 {
                    mac_glyph_char(idx)
                } else {
                    names
                        .get(idx as usize - 258)
                        .and_then(|n| super::glyphlist::glyph_to_unicode(n))
                };
                if let Some(ch) = ch {
                    out[c] = ch as u32;
                    mapped = true;
                }
            }
        }
    }

    mapped.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_format12_group_is_rejected_not_partially_accepted() {
        let mut sub = vec![0u8; 28];
        sub[0..2].copy_from_slice(&12u16.to_be_bytes());
        sub[12..16].copy_from_slice(&1u32.to_be_bytes());
        sub[16..20].copy_from_slice(&0x1_0000u32.to_be_bytes());
        sub[20..24].copy_from_slice(&0x2_0000u32.to_be_bytes());
        sub[24..28].copy_from_slice(&1u32.to_be_bytes());
        assert!(walk_subtable(&sub, |_, _| {}).is_none());
    }

    #[test]
    fn truncated_subtable_returns_none_without_panicking() {
        assert!(walk_subtable(&[0, 4], |_, _| {}).is_none());
        assert!(walk_subtable(&[0, 12, 0], |_, _| {}).is_none());
    }
}
