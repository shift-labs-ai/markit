//! Font resolution: everything that turns a PDF font dictionary into
//! a decode-ready FontInfo — metrics (Widths, standard-14 AFM, CID W
//! arrays, Type3 FontMatrix), simple-font encodings, ToUnicode CMaps,
//! embedded font-program unicode recovery (TrueType, Type1, CFF), and
//! the predefined/embedded CMaps of composite fonts.

pub(crate) mod cjk_cmap;
mod encoding;
mod glyphlist;
mod tounicode;
pub(crate) mod truetype;
pub(crate) mod type1;

use rustc_hash::FxHashMap;

use super::own_pdf::{decode_stream, dget, Dict, Pdf, Val};
use encoding::build_simple_encoding;
use tounicode::parse_tounicode;

pub(crate) struct FontInfo {
    /// Advance widths in glyph space /1000. Simple fonts index by byte;
    /// CID fonts consult `cid_widths`.
    pub(crate) widths: [f64; 256],
    pub(crate) cid_widths: FxHashMap<u32, f64>,
    pub(crate) default_width: f64,
    /// Unicode per byte (simple) — from ToUnicode or the font encoding.
    pub(crate) to_unicode_simple: [Option<char>; 256],
    /// Unicode per CID/code (composite fonts, or multi-char mappings).
    pub(crate) to_unicode: FxHashMap<u32, String>,
    /// Two-byte codes (Type0/Identity-H).
    pub(crate) two_byte: bool,
    pub(crate) is_bold: bool,
    pub(crate) size_hint_monospace: bool,
    pub(crate) symbol_font: SymbolFont,
    /// Codes ARE UCS-2 code units (UniXX-UCS2-H/V predefined CMaps).
    pub(crate) ucs2_codes: bool,
    /// Unsupported predefined CMap: text through this font cannot be
    /// decoded — the page must go to the fallback engine.
    pub(crate) unsupported_cmap: bool,
    /// Predefined CJK CMap (GBK-EUC-H and friends): variable-length
    /// codes to CIDs, with the ordering's CID->Unicode table.
    pub(crate) cjk: Option<std::sync::Arc<cjk_cmap::CjkCmap>>,
    /// Adobe CID ordering table for Identity-encoded CID-keyed fonts
    /// without ToUnicode.
    pub(crate) adobe_ordering: Option<cjk_cmap::OrderingMap>,
    /// Vertical writing mode (WMode 1: -V CMaps). Glyphs advance down
    /// the page instead of across it.
    pub(crate) vertical: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SymbolFont {
    #[default]
    None,
    Symbol,
    ZapfDingbats,
}

impl Default for FontInfo {
    fn default() -> Self {
        FontInfo {
            widths: [0.0; 256],
            cid_widths: FxHashMap::default(),
            default_width: 500.0,
            to_unicode_simple: [None; 256],
            to_unicode: FxHashMap::default(),
            two_byte: false,
            is_bold: false,
            symbol_font: SymbolFont::None,
            ucs2_codes: false,
            unsupported_cmap: false,
            cjk: None,
            adobe_ordering: None,
            vertical: false,
            size_hint_monospace: false,
        }
    }
}

pub(crate) fn build_font(pdf: &Pdf, dict: &Dict) -> FontInfo {
    let mut info = FontInfo::default();
    let g = |key: &[u8]| pdf.dict_get(dict, key).ok().flatten();

    let base_name = g(b"BaseFont")
        .and_then(|v| {
            v.as_name()
                .map(|n| String::from_utf8_lossy(n).to_lowercase())
        })
        .unwrap_or_default();
    info.is_bold =
        base_name.contains("bold") || base_name.contains("black") || base_name.contains("heavy");
    info.size_hint_monospace = base_name.contains("courier") || base_name.contains("mono");
    if base_name.contains("symbol") {
        info.symbol_font = SymbolFont::Symbol;
    } else if base_name.contains("zapf") || base_name.contains("dingbat") {
        info.symbol_font = SymbolFont::ZapfDingbats;
    }

    let is_type0 = matches!(g(b"Subtype"), Some(Val::Name(b"Type0")));

    if is_type0 {
        info.two_byte = true;
        match g(b"Encoding") {
            // Identity: codes are CIDs. Vertical variants extract the
            // same text; assembly treats them as horizontal runs.
            Some(Val::Name(b"Identity-H")) | None => {}
            Some(Val::Name(b"Identity-V")) => info.vertical = true,
            Some(Val::Name(n)) => {
                info.vertical = n.ends_with(b"-V");
                if n.ends_with(b"UCS2-H")
                    || n.ends_with(b"UCS2-V")
                    || n.ends_with(b"UTF16-H")
                    || n.ends_with(b"UTF16-V")
                {
                    // Codes are UCS-2/UTF-16 code units directly.
                    info.ucs2_codes = true;
                } else if let Some(cm) = cjk_cmap::lookup(n) {
                    info.cjk = Some(cm);
                } else {
                    // Not in Adobe's published set: refuse rather than
                    // emit garbage.
                    info.unsupported_cmap = true;
                }
            }
            // Embedded CMap stream: parse its codespace + cidranges
            // (unicode joins later from ToUnicode or the ordering).
            Some(Val::Stream(sd, raw)) => {
                if let Ok(text) = decode_stream(&sd, raw, pdf) {
                    info.vertical = memchr::memmem::find(&text, b"/WMode 1").is_some();
                    info.cjk = cjk_cmap::parse_embedded(&text, None);
                }
            }
            _ => {}
        }
        if let Some(Val::Array(desc)) = g(b"DescendantFonts") {
            if let Some(d0) = desc.first() {
                if let Ok(Val::Dict(cid_font)) = pdf.resolve(d0) {
                    let cg = |key: &[u8]| pdf.dict_get(&cid_font, key).ok().flatten();
                    info.default_width = cg(b"DW").and_then(|v| v.as_num()).unwrap_or(1000.0);
                    if let Some(Val::Array(w)) = cg(b"W") {
                        parse_cid_widths(pdf, &w, &mut info.cid_widths);
                    }
                    if !info.is_bold {
                        info.is_bold = descriptor_bold(pdf, &cid_font);
                    }
                    // No ToUnicode on the parent: recover CID->unicode
                    // through the descendant's font program (gid->unicode
                    // from the inverted cmap; CID->gid via CIDToGIDMap,
                    // Identity in the common case).
                    if g(b"ToUnicode").is_none() {
                        recover_cid_unicode(pdf, &cid_font, &mut info);
                    }
                    // Adobe CID-keyed font: the ordering's CID->Unicode
                    // table applies directly (fills the gaps the font
                    // program left).
                    if g(b"ToUnicode").is_none() {
                        if let Ok(Some(Val::Dict(csi))) = pdf.dict_get(&cid_font, b"CIDSystemInfo")
                        {
                            let ord = match pdf.dict_get(&csi, b"Ordering") {
                                Ok(Some(Val::Name(n))) => Some(n.to_vec()),
                                Ok(Some(Val::Str(s))) => Some(s),
                                _ => None,
                            };
                            if let Some(ord) = ord {
                                info.adobe_ordering = cjk_cmap::ordering_map(&ord);
                            }
                        }
                    }
                }
            }
        }
    } else {
        info.default_width = 0.0;
        let first_char = g(b"FirstChar").and_then(|v| v.as_num()).unwrap_or(0.0) as usize;
        if let Some(Val::Array(w)) = g(b"Widths") {
            for (i, o) in w.iter().enumerate() {
                if let Some(v) = pdf.resolve(o).ok().and_then(|v| v.as_num()) {
                    if first_char + i < 256 {
                        info.widths[first_char + i] = v;
                    }
                }
            }
        } else {
            // No Widths: standard-14 territory. Real AFM metrics for the
            // Helvetica/Times/Courier families; sensible fill elsewhere.
            standard14_widths(&base_name, &mut info.widths);
        }
        // Type3 widths are expressed in glyph space: FontMatrix maps them
        // to text space (nominally /1000 units for other font types).
        if matches!(g(b"Subtype"), Some(Val::Name(b"Type3"))) {
            if let Some(Val::Array(fm)) = g(b"FontMatrix") {
                if let Some(a) = fm.first().and_then(|v| pdf.resolve(v).ok()?.as_num()) {
                    let scale = a * 1000.0;
                    for w in info.widths.iter_mut() {
                        *w *= scale;
                    }
                }
            }
        }
        if !info.is_bold {
            info.is_bold = descriptor_bold(pdf, dict);
        }
        build_simple_encoding(pdf, dict, &mut info);
    }

    // ToUnicode overrides encoding-derived mappings.
    let mut has_tounicode = false;
    if let Some(Val::Stream(sd, raw)) = g(b"ToUnicode") {
        if let Ok(data) = decode_stream(&sd, raw, pdf) {
            parse_tounicode(&data, &mut info);
            has_tounicode = true;
        }
    }

    // No ToUnicode and no /Encoding: the codes are font-specific and the
    // WinAnsi default is a guess. The embedded TrueType program knows the
    // truth — recover code->unicode from its cmap/post tables.
    if !has_tounicode && !is_type0 {
        let has_encoding = match g(b"Encoding") {
            Some(Val::Name(_)) => true,
            Some(Val::Dict(d)) => dget(&d, b"Differences").is_some(),
            _ => false,
        };
        if !has_encoding {
            if let Some(Val::Dict(fd)) = g(b"FontDescriptor") {
                let program_map = |key: &[u8]| -> Option<[u32; 256]> {
                    let Ok(Some(Val::Stream(sd, raw))) = pdf.dict_get(&fd, key) else {
                        return None;
                    };
                    let program = decode_stream(&sd, raw, pdf).ok()?;
                    match key {
                        b"FontFile2" => truetype::code_to_unicode(&program),
                        b"FontFile" => type1::type1_code_to_unicode(&program),
                        _ => type1::cff_code_to_unicode(&program),
                    }
                };
                let map = program_map(b"FontFile2")
                    .or_else(|| program_map(b"FontFile"))
                    .or_else(|| program_map(b"FontFile3"));
                if let Some(map) = map {
                    for (code, &u) in map.iter().enumerate() {
                        if u != 0 {
                            info.to_unicode_simple[code] = char::from_u32(u);
                        }
                    }
                }
            }
        }
    }

    info
}

/// AFM widths for the standard-14 text fonts, ASCII 32..=126 (the range
/// that drives line assembly). Codes outside get the font's average.
fn standard14_widths(base: &str, widths: &mut [f64; 256]) {
    const HELV: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
        722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
        556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
        500, 334, 260, 334, 584,
    ];
    const HELV_B: [u16; 95] = [
        278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722,
        722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611,
        611, 278, 278, 556, 278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556,
        500, 389, 280, 389, 584,
    ];
    const TIMES: [u16; 95] = [
        250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667,
        722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722,
        722, 944, 722, 722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500,
        500, 278, 278, 500, 278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500,
        444, 480, 200, 480, 541,
    ];
    const TIMES_B: [u16; 95] = [
        250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 930, 722, 667, 722,
        722, 667, 611, 778, 778, 389, 500, 778, 667, 944, 722, 778, 611, 778, 722, 556, 667, 722,
        722, 1000, 722, 722, 667, 333, 278, 333, 581, 500, 333, 500, 556, 444, 556, 444, 333, 500,
        556, 278, 333, 556, 278, 833, 556, 500, 556, 556, 444, 389, 333, 556, 500, 722, 500, 500,
        444, 394, 220, 394, 520,
    ];
    const TIMES_I: [u16; 95] = [
        250, 333, 420, 500, 500, 833, 778, 214, 333, 333, 500, 675, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 675, 675, 675, 500, 920, 611, 611, 667,
        722, 611, 611, 722, 722, 333, 444, 667, 556, 833, 667, 722, 611, 722, 611, 500, 556, 722,
        611, 833, 611, 556, 556, 389, 278, 389, 422, 500, 333, 500, 500, 444, 500, 444, 278, 500,
        500, 278, 278, 444, 278, 722, 500, 500, 500, 500, 389, 389, 278, 500, 444, 667, 444, 444,
        389, 400, 275, 400, 541,
    ];
    const TIMES_BI: [u16; 95] = [
        250, 389, 555, 500, 500, 833, 778, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 832, 667, 667, 667,
        722, 667, 667, 722, 778, 389, 500, 667, 611, 889, 722, 722, 611, 722, 667, 556, 611, 722,
        667, 889, 667, 611, 611, 333, 278, 333, 570, 500, 333, 500, 500, 444, 500, 444, 333, 500,
        556, 278, 278, 500, 278, 778, 556, 500, 500, 500, 389, 389, 278, 556, 444, 667, 500, 444,
        389, 348, 220, 348, 570,
    ];

    let bold = base.contains("bold");
    let italic = base.contains("italic") || base.contains("oblique");
    let (table, avg): (&[u16; 95], f64) = if base.contains("courier") || base.contains("mono") {
        *widths = [600.0; 256];
        return;
    } else if base.contains("times") || base.contains("roman") || base.contains("serif") {
        match (bold, italic) {
            (true, true) => (&TIMES_BI, 500.0),
            (true, false) => (&TIMES_B, 500.0),
            (false, true) => (&TIMES_I, 500.0),
            (false, false) => (&TIMES, 500.0),
        }
    } else {
        // Helvetica/Arial and anything unknown: sans metrics.
        if bold {
            (&HELV_B, 556.0)
        } else {
            (&HELV, 556.0)
        }
    };
    *widths = [avg; 256];
    for (i, &w) in table.iter().enumerate() {
        widths[32 + i] = w as f64;
    }
}

fn descriptor_bold(pdf: &Pdf, font_dict: &Dict) -> bool {
    const FORCE_BOLD: i64 = 1 << 18;
    if let Ok(Some(Val::Dict(fd))) = pdf.dict_get(font_dict, b"FontDescriptor") {
        if let Ok(Some(Val::Num(flags))) = pdf.dict_get(&fd, b"Flags") {
            if (flags as i64) & FORCE_BOLD != 0 {
                return true;
            }
        }
        if let Ok(Some(v)) = pdf.dict_get(&fd, b"StemV") {
            if let Some(v) = v.as_num() {
                return v >= 140.0;
            }
        }
    }
    false
}

fn parse_cid_widths(pdf: &Pdf, w: &[Val], out: &mut FxHashMap<u32, f64>) {
    // W format: [ c [w1 w2 …] ] or [ c_first c_last w ]
    let mut i = 0;
    while i < w.len() {
        let Some(first) = pdf.resolve(&w[i]).ok().and_then(|v| v.as_num()) else {
            break;
        };
        match w.get(i + 1).and_then(|o| pdf.resolve(o).ok()) {
            Some(Val::Array(list)) => {
                for (j, o) in list.iter().enumerate() {
                    if let Some(v) = pdf.resolve(o).ok().and_then(|v| v.as_num()) {
                        out.insert(first as u32 + j as u32, v);
                    }
                }
                i += 2;
            }
            Some(other) => {
                let Some(last) = other.as_num() else { break };
                let Some(v) = w
                    .get(i + 2)
                    .and_then(|o| pdf.resolve(o).ok())
                    .and_then(|v| v.as_num())
                else {
                    break;
                };
                for c in first as u32..=last as u32 {
                    out.insert(c, v);
                }
                i += 3;
            }
            None => break,
        }
    }
}

fn recover_cid_unicode(pdf: &Pdf, cid_font: &Dict, info: &mut FontInfo) {
    let Ok(Some(Val::Dict(fd))) = pdf.dict_get(cid_font, b"FontDescriptor") else {
        return;
    };
    let Ok(Some(Val::Stream(sd, raw))) = pdf.dict_get(&fd, b"FontFile2") else {
        return;
    };
    let Ok(program) = decode_stream(&sd, raw, pdf) else {
        return;
    };
    let Some(gid_uni) = truetype::gid_to_unicode(&program) else {
        return;
    };

    match pdf.dict_get(cid_font, b"CIDToGIDMap") {
        Ok(Some(Val::Stream(md, mraw))) => {
            if let Ok(map) = decode_stream(&md, mraw, pdf) {
                for (cid, gb) in map.chunks_exact(2).enumerate() {
                    let gid = u16::from_be_bytes([gb[0], gb[1]]);
                    if let Some(&u) = gid_uni.get(&gid) {
                        if let Some(c) = char::from_u32(u) {
                            info.to_unicode
                                .entry(cid as u32)
                                .or_insert_with(|| c.to_string());
                        }
                    }
                }
            }
        }
        _ => {
            // Identity (the default): cid == gid.
            for (&gid, &u) in &gid_uni {
                if let Some(c) = char::from_u32(u) {
                    info.to_unicode
                        .entry(gid as u32)
                        .or_insert_with(|| c.to_string());
                }
            }
        }
    }
}
