//! markit's PDF extraction engine: a pure-Rust content-stream
//! interpreter over the own_pdf object layer, producing the same
//! PageContent shape the MuPDF path produces at a fraction of the cost.
//! MuPDF remains the rasterizer (render_image_region) and the fallback
//! for anything this engine cannot handle faithfully (see extract_pages).
//!
//! Coordinates: PDF user space is bottom-left/y-up, which is what the
//! downstream pipeline consumes for text boxes and segments. Image
//! regions keep the MuPDF path's device-space (y-down) convention.

use anyhow::{anyhow, bail, Result};
use rustc_hash::{FxHashMap, FxHashSet};

use super::own_pdf::{decode_stream, dget, Dict, Pdf, Val};

use super::types::{PageContent, Segment};

// ── 2D affine matrix [a b c d e f] ──────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub(crate) struct Mat {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

pub(crate) const IDENTITY: Mat = Mat {
    a: 1.0,
    b: 0.0,
    c: 0.0,
    d: 1.0,
    e: 0.0,
    f: 0.0,
};

impl Mat {
    fn mul(self, m: Mat) -> Mat {
        // self × m (apply self first, then m)
        Mat {
            a: self.a * m.a + self.b * m.c,
            b: self.a * m.b + self.b * m.d,
            c: self.c * m.a + self.d * m.c,
            d: self.c * m.b + self.d * m.d,
            e: self.e * m.a + self.f * m.c + m.e,
            f: self.e * m.b + self.f * m.d + m.f,
        }
    }

    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Magnitude of the y-axis scale (for font size in device space).
    fn y_scale(&self) -> f64 {
        (self.c * self.c + self.d * self.d).sqrt()
    }
}

// ── Fonts ────────────────────────────────────────────────────────────────────

pub(crate) struct FontInfo {
    /// Advance widths in glyph space /1000. Simple fonts index by byte;
    /// CID fonts consult `cid_widths`.
    widths: [f64; 256],
    cid_widths: FxHashMap<u32, f64>,
    default_width: f64,
    /// Unicode per byte (simple) — from ToUnicode or the font encoding.
    to_unicode_simple: [Option<char>; 256],
    /// Unicode per CID/code (composite fonts, or multi-char mappings).
    to_unicode: FxHashMap<u32, String>,
    /// Two-byte codes (Type0/Identity-H).
    two_byte: bool,
    is_bold: bool,
    size_hint_monospace: bool,
    symbol_font: SymbolFont,
    /// Codes ARE UCS-2 code units (UniXX-UCS2-H/V predefined CMaps).
    ucs2_codes: bool,
    /// Unsupported predefined CMap: text through this font cannot be
    /// decoded — the page must go to the fallback engine.
    unsupported_cmap: bool,
    /// Predefined CJK CMap (GBK-EUC-H and friends): variable-length
    /// codes to CIDs, with the ordering's CID->Unicode table.
    cjk: Option<std::sync::Arc<super::cjk_cmap::CjkCmap>>,
    /// Adobe CID ordering table for Identity-encoded CID-keyed fonts
    /// without ToUnicode.
    adobe_ordering: Option<super::cjk_cmap::OrderingMap>,
    /// Vertical writing mode (WMode 1: -V CMaps). Glyphs advance down
    /// the page instead of across it.
    vertical: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SymbolFont {
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

fn build_font(pdf: &Pdf, dict: &Dict) -> FontInfo {
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
                } else if let Some(cm) = super::cjk_cmap::lookup(n) {
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
                    info.cjk = super::cjk_cmap::parse_embedded(&text, None);
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
                                info.adobe_ordering = super::cjk_cmap::ordering_map(&ord);
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
                if let Ok(Some(Val::Stream(sd, raw))) = pdf.dict_get(&fd, b"FontFile2") {
                    if let Ok(program) = decode_stream(&sd, raw, pdf) {
                        let map = super::truetype::code_to_unicode(&program);
                        if std::env::var("MARKIT_DEBUG").is_ok() {
                            eprintln!(
                                "[tt-recover] {} {}",
                                base_name,
                                if map.is_some() { "OK" } else { "MISS" }
                            );
                        }
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

// ── Encodings ────────────────────────────────────────────────────────────────

/// MacRomanEncoding, code points 128–255 (0 = unmapped).
const MAC_ROMAN_HIGH: [u32; 128] = [
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1, 0x00E0, 0x00E2, 0x00E4, 0x00E3,
    0x00E5, 0x00E7, 0x00E9, 0x00E8, 0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3,
    0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC, 0x2020, 0x00B0, 0x00A2, 0x00A3,
    0x00A7, 0x2022, 0x00B6, 0x00DF, 0x00AE, 0x00A9, 0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8,
    0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211, 0x220F, 0x03C0, 0x222B, 0x00AA,
    0x00BA, 0x03A9, 0x00E6, 0x00F8, 0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB,
    0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153, 0x2013, 0x2014, 0x201C, 0x201D,
    0x2018, 0x2019, 0x00F7, 0x25CA, 0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02,
    0x2021, 0x00B7, 0x201A, 0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1, 0x00CB, 0x00C8, 0x00CD, 0x00CE,
    0x00CF, 0x00CC, 0x00D3, 0x00D4, 0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC,
    0x00AF, 0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
];

/// StandardEncoding (Adobe), code points 128–255 (0 = unmapped).
const STANDARD_HIGH: [u32; 128] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0x00A1, 0x00A2, 0x00A3, 0x2044, 0x00A5, 0x0192, 0x00A7, 0x00A4, 0x0027, 0x201C, 0x00AB,
    0x2039, 0x203A, 0xFB01, 0xFB02, 0, 0x2013, 0x2020, 0x2021, 0x00B7, 0, 0x00B6, 0x2022, 0x201A,
    0x201E, 0x201D, 0x00BB, 0x2026, 0x2030, 0, 0x00BF, 0, 0x0060, 0x00B4, 0x02C6, 0x02DC, 0x00AF,
    0x02D8, 0x02D9, 0x00A8, 0, 0x02DA, 0x00B8, 0, 0x02DD, 0x02DB, 0x02C7, 0x2014, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x00C6, 0, 0x00AA, 0, 0, 0, 0, 0x0141, 0x00D8, 0x0152, 0x00BA, 0,
    0, 0, 0, 0, 0x00E6, 0, 0, 0, 0x0131, 0, 0, 0x0142, 0x00F8, 0x0153, 0x00DF, 0, 0, 0, 0,
];

/// WinAnsiEncoding, code points 128–255 (0 = unmapped).
const WIN_ANSI_HIGH: [u32; 128] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
    0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178, 0x00A0, 0x00A1, 0x00A2, 0x00A3, 0x00A4,
    0x00A5, 0x00A6, 0x00A7, 0x00A8, 0x00A9, 0x00AA, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x00AF, 0x00B0,
    0x00B1, 0x00B2, 0x00B3, 0x00B4, 0x00B5, 0x00B6, 0x00B7, 0x00B8, 0x00B9, 0x00BA, 0x00BB, 0x00BC,
    0x00BD, 0x00BE, 0x00BF, 0x00C0, 0x00C1, 0x00C2, 0x00C3, 0x00C4, 0x00C5, 0x00C6, 0x00C7, 0x00C8,
    0x00C9, 0x00CA, 0x00CB, 0x00CC, 0x00CD, 0x00CE, 0x00CF, 0x00D0, 0x00D1, 0x00D2, 0x00D3, 0x00D4,
    0x00D5, 0x00D6, 0x00D7, 0x00D8, 0x00D9, 0x00DA, 0x00DB, 0x00DC, 0x00DD, 0x00DE, 0x00DF, 0x00E0,
    0x00E1, 0x00E2, 0x00E3, 0x00E4, 0x00E5, 0x00E6, 0x00E7, 0x00E8, 0x00E9, 0x00EA, 0x00EB, 0x00EC,
    0x00ED, 0x00EE, 0x00EF, 0x00F0, 0x00F1, 0x00F2, 0x00F3, 0x00F4, 0x00F5, 0x00F6, 0x00F7, 0x00F8,
    0x00F9, 0x00FA, 0x00FB, 0x00FC, 0x00FD, 0x00FE, 0x00FF,
];

/// Adobe Glyph List, the practical subset: Latin with full diacritics,
/// ligatures, punctuation, Greek, and common math/symbol names — the
/// names Differences arrays actually use. Suffixed variants
/// ("a.sc", "one.oldstyle") fall back to their base name.
fn glyph_to_unicode(name: &[u8]) -> Option<char> {
    let s = std::str::from_utf8(name).ok()?;
    // uniXXXX / uXXXX[XX]
    if let Some(hex) = s.strip_prefix("uni").or_else(|| s.strip_prefix("u")) {
        if hex.len() >= 4 {
            if let Ok(v) = u32::from_str_radix(&hex[..4], 16) {
                return char::from_u32(v);
            }
        }
    }
    if let Some(c) = agl_lookup(s) {
        return Some(c);
    }
    // Variant suffix ("eacute.sc", "one.onum") → base glyph.
    if let Some(dot) = s.find('.') {
        return agl_lookup(&s[..dot]);
    }
    None
}

/// Public shim for the truetype module's post-table name lookup.
pub fn glyph_to_unicode_pub(name: &[u8]) -> Option<char> {
    glyph_to_unicode(name)
}

fn agl_lookup(s: &str) -> Option<char> {
    Some(match s {
        // ── ASCII ────────────────────────────────────────────────
        "space" => ' ',
        "exclam" => '!',
        "quotedbl" => '"',
        "numbersign" => '#',
        "dollar" => '$',
        "percent" => '%',
        "ampersand" => '&',
        "quotesingle" => '\'',
        "parenleft" => '(',
        "parenright" => ')',
        "asterisk" => '*',
        "plus" => '+',
        "comma" => ',',
        "hyphen" => '-',
        "period" => '.',
        "slash" => '/',
        "zero" => '0',
        "one" => '1',
        "two" => '2',
        "three" => '3',
        "four" => '4',
        "five" => '5',
        "six" => '6',
        "seven" => '7',
        "eight" => '8',
        "nine" => '9',
        "colon" => ':',
        "semicolon" => ';',
        "less" => '<',
        "equal" => '=',
        "greater" => '>',
        "question" => '?',
        "at" => '@',
        "bracketleft" => '[',
        "backslash" => '\\',
        "bracketright" => ']',
        "asciicircum" => '^',
        "underscore" => '_',
        "grave" => '\u{60}',
        "braceleft" => '{',
        "bar" => '|',
        "braceright" => '}',
        "asciitilde" => '~',
        // ── quotes, dashes, marks ────────────────────────────────
        "quoteleft" => '\u{2018}',
        "quoteright" => '\u{2019}',
        "quotesinglbase" => '\u{201A}',
        "quotedblleft" => '\u{201C}',
        "quotedblright" => '\u{201D}',
        "quotedblbase" => '\u{201E}',
        "guilsinglleft" => '\u{2039}',
        "guilsinglright" => '\u{203A}',
        "guillemotleft" => '\u{00AB}',
        "guillemotright" => '\u{00BB}',
        "endash" => '\u{2013}',
        "emdash" => '\u{2014}',
        "bullet" => '\u{2022}',
        "ellipsis" => '\u{2026}',
        "dagger" => '\u{2020}',
        "daggerdbl" => '\u{2021}',
        "perthousand" => '\u{2030}',
        "periodcentered" => '\u{00B7}',
        "exclamdown" => '\u{00A1}',
        "questiondown" => '\u{00BF}',
        "section" => '\u{00A7}',
        "paragraph" => '\u{00B6}',
        "fraction" => '\u{2044}',
        "minute" => '\u{2032}',
        "second" => '\u{2033}',
        // ── currency & signs ─────────────────────────────────────
        "cent" => '\u{00A2}',
        "sterling" => '\u{00A3}',
        "yen" => '\u{00A5}',
        "florin" => '\u{0192}',
        "currency" => '\u{00A4}',
        "Euro" | "euro" => '\u{20AC}',
        "copyright" => '\u{00A9}',
        "registered" => '\u{00AE}',
        "trademark" => '\u{2122}',
        "degree" => '\u{00B0}',
        "plusminus" => '\u{00B1}',
        "multiply" => '\u{00D7}',
        "divide" => '\u{00F7}',
        "minus" => '\u{2212}',
        "logicalnot" => '\u{00AC}',
        "mu" => '\u{00B5}',
        "micro" => '\u{00B5}',
        // ── ligatures & special latin ────────────────────────────
        "fi" => '\u{FB01}',
        "fl" => '\u{FB02}',
        "ff" => '\u{FB00}',
        "ffi" => '\u{FB03}',
        "ffl" => '\u{FB04}',
        "ae" => '\u{00E6}',
        "AE" => '\u{00C6}',
        "oe" => '\u{0153}',
        "OE" => '\u{0152}',
        "oslash" => '\u{00F8}',
        "Oslash" => '\u{00D8}',
        "germandbls" => '\u{00DF}',
        "dotlessi" => '\u{0131}',
        "thorn" => '\u{00FE}',
        "Thorn" => '\u{00DE}',
        "eth" => '\u{00F0}',
        "Eth" => '\u{00D0}',
        "Lslash" => '\u{0141}',
        "lslash" => '\u{0142}',
        // ── accented Latin (lowercase) ───────────────────────────
        "agrave" => '\u{00E0}',
        "aacute" => '\u{00E1}',
        "acircumflex" => '\u{00E2}',
        "atilde" => '\u{00E3}',
        "adieresis" => '\u{00E4}',
        "aring" => '\u{00E5}',
        "ccedilla" => '\u{00E7}',
        "egrave" => '\u{00E8}',
        "eacute" => '\u{00E9}',
        "ecircumflex" => '\u{00EA}',
        "edieresis" => '\u{00EB}',
        "igrave" => '\u{00EC}',
        "iacute" => '\u{00ED}',
        "icircumflex" => '\u{00EE}',
        "idieresis" => '\u{00EF}',
        "ntilde" => '\u{00F1}',
        "ograve" => '\u{00F2}',
        "oacute" => '\u{00F3}',
        "ocircumflex" => '\u{00F4}',
        "otilde" => '\u{00F5}',
        "odieresis" => '\u{00F6}',
        "ugrave" => '\u{00F9}',
        "uacute" => '\u{00FA}',
        "ucircumflex" => '\u{00FB}',
        "udieresis" => '\u{00FC}',
        "yacute" => '\u{00FD}',
        "ydieresis" => '\u{00FF}',
        "scaron" => '\u{0161}',
        "zcaron" => '\u{017E}',
        "ccaron" => '\u{010D}',
        "rcaron" => '\u{0159}',
        "ecaron" => '\u{011B}',
        "abreve" => '\u{0103}',
        "amacron" => '\u{0101}',
        "aogonek" => '\u{0105}',
        "eogonek" => '\u{0119}',
        "cacute" => '\u{0107}',
        "nacute" => '\u{0144}',
        "sacute" => '\u{015B}',
        "zacute" => '\u{017A}',
        "zdotaccent" => '\u{017C}',
        // ── accented Latin (uppercase) ───────────────────────────
        "Agrave" => '\u{00C0}',
        "Aacute" => '\u{00C1}',
        "Acircumflex" => '\u{00C2}',
        "Atilde" => '\u{00C3}',
        "Adieresis" => '\u{00C4}',
        "Aring" => '\u{00C5}',
        "Ccedilla" => '\u{00C7}',
        "Egrave" => '\u{00C8}',
        "Eacute" => '\u{00C9}',
        "Ecircumflex" => '\u{00CA}',
        "Edieresis" => '\u{00CB}',
        "Igrave" => '\u{00CC}',
        "Iacute" => '\u{00CD}',
        "Icircumflex" => '\u{00CE}',
        "Idieresis" => '\u{00CF}',
        "Ntilde" => '\u{00D1}',
        "Ograve" => '\u{00D2}',
        "Oacute" => '\u{00D3}',
        "Ocircumflex" => '\u{00D4}',
        "Otilde" => '\u{00D5}',
        "Odieresis" => '\u{00D6}',
        "Ugrave" => '\u{00D9}',
        "Uacute" => '\u{00DA}',
        "Ucircumflex" => '\u{00DB}',
        "Udieresis" => '\u{00DC}',
        "Yacute" => '\u{00DD}',
        "Scaron" => '\u{0160}',
        "Zcaron" => '\u{017D}',
        // ── accents (spacing) ────────────────────────────────────
        "circumflex" => '\u{02C6}',
        "caron" => '\u{02C7}',
        "breve" => '\u{02D8}',
        "dotaccent" => '\u{02D9}',
        "ring" => '\u{02DA}',
        "ogonek" => '\u{02DB}',
        "tilde" => '\u{02DC}',
        "hungarumlaut" => '\u{02DD}',
        "macron" => '\u{00AF}',
        "acute" => '\u{00B4}',
        "cedilla" => '\u{00B8}',
        "dieresis" => '\u{00A8}',
        // ── ordinals & fractions ─────────────────────────────────
        "ordfeminine" => '\u{00AA}',
        "ordmasculine" => '\u{00BA}',
        "onequarter" => '\u{00BC}',
        "onehalf" => '\u{00BD}',
        "threequarters" => '\u{00BE}',
        "onesuperior" => '\u{00B9}',
        "twosuperior" => '\u{00B2}',
        "threesuperior" => '\u{00B3}',
        // ── Greek ────────────────────────────────────────────────
        "alpha" => '\u{03B1}',
        "beta" => '\u{03B2}',
        "gamma" => '\u{03B3}',
        "delta" => '\u{03B4}',
        "epsilon" => '\u{03B5}',
        "zeta" => '\u{03B6}',
        "eta" => '\u{03B7}',
        "theta" => '\u{03B8}',
        "iota" => '\u{03B9}',
        "kappa" => '\u{03BA}',
        "lambda" => '\u{03BB}',
        "nu" => '\u{03BD}',
        "xi" => '\u{03BE}',
        "omicron" => '\u{03BF}',
        "pi" => '\u{03C0}',
        "rho" => '\u{03C1}',
        "sigma" => '\u{03C3}',
        "sigma1" => '\u{03C2}',
        "tau" => '\u{03C4}',
        "upsilon" => '\u{03C5}',
        "phi" => '\u{03C6}',
        "chi" => '\u{03C7}',
        "psi" => '\u{03C8}',
        "omega" => '\u{03C9}',
        "Gamma" => '\u{0393}',
        "Delta" => '\u{0394}',
        "Theta" => '\u{0398}',
        "Lambda" => '\u{039B}',
        "Xi" => '\u{039E}',
        "Pi" => '\u{03A0}',
        "Sigma" => '\u{03A3}',
        "Upsilon" => '\u{03A5}',
        "Phi" => '\u{03A6}',
        "Psi" => '\u{03A8}',
        "Omega" => '\u{03A9}',
        // ── math & misc symbols ──────────────────────────────────
        "infinity" => '\u{221E}',
        "partialdiff" => '\u{2202}',
        "summation" => '\u{2211}',
        "product" => '\u{220F}',
        "integral" => '\u{222B}',
        "radical" => '\u{221A}',
        "approxequal" => '\u{2248}',
        "notequal" => '\u{2260}',
        "lessequal" => '\u{2264}',
        "greaterequal" => '\u{2265}',
        "equivalence" => '\u{2261}',
        "element" => '\u{2208}',
        "intersection" => '\u{2229}',
        "union" => '\u{222A}',
        "arrowleft" => '\u{2190}',
        "arrowup" => '\u{2191}',
        "arrowright" => '\u{2192}',
        "arrowdown" => '\u{2193}',
        "arrowboth" => '\u{2194}',
        "lozenge" => '\u{25CA}',
        "diamond" => '\u{2666}',
        "heart" => '\u{2665}',
        "spade" => '\u{2660}',
        "club" => '\u{2663}',
        "brokenbar" => '\u{00A6}',
        "nbspace" => '\u{00A0}',
        "sfthyphen" => '\u{00AD}',
        "apple" => '\u{F8FF}',
        other => {
            // Single-letter names map to themselves (A–Z, a–z).
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => c,
                _ => return None,
            }
        }
    })
}

/// CIDFontType2 without ToUnicode: read the descendant's TrueType
/// program, invert its unicode cmap, and translate CIDs through
/// CIDToGIDMap (Identity, or the explicit 2-bytes-per-CID stream).
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
    let Some(gid_uni) = super::truetype::gid_to_unicode(&program) else {
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

/// Symbol font built-in encoding (Greek + math, the used range).
fn symbol_encoding(map: &mut [Option<char>; 256]) {
    *map = [None; 256];
    const PAIRS: &[(u8, u32)] = &[
        (0x20, 0x0020),
        (0x21, 0x0021),
        (0x22, 0x2200),
        (0x23, 0x0023),
        (0x24, 0x2203),
        (0x25, 0x0025),
        (0x26, 0x0026),
        (0x27, 0x220B),
        (0x28, 0x0028),
        (0x29, 0x0029),
        (0x2A, 0x2217),
        (0x2B, 0x002B),
        (0x2C, 0x002C),
        (0x2D, 0x2212),
        (0x2E, 0x002E),
        (0x2F, 0x002F),
        (0x3A, 0x003A),
        (0x3B, 0x003B),
        (0x3C, 0x003C),
        (0x3D, 0x003D),
        (0x3E, 0x003E),
        (0x3F, 0x003F),
        (0x40, 0x2245),
        (0x41, 0x0391),
        (0x42, 0x0392),
        (0x43, 0x03A7),
        (0x44, 0x0394),
        (0x45, 0x0395),
        (0x46, 0x03A6),
        (0x47, 0x0393),
        (0x48, 0x0397),
        (0x49, 0x0399),
        (0x4A, 0x03D1),
        (0x4B, 0x039A),
        (0x4C, 0x039B),
        (0x4D, 0x039C),
        (0x4E, 0x039D),
        (0x4F, 0x039F),
        (0x50, 0x03A0),
        (0x51, 0x0398),
        (0x52, 0x03A1),
        (0x53, 0x03A3),
        (0x54, 0x03A4),
        (0x55, 0x03A5),
        (0x56, 0x03C2),
        (0x57, 0x03A9),
        (0x58, 0x039E),
        (0x59, 0x03A8),
        (0x5A, 0x0396),
        (0x5B, 0x005B),
        (0x5C, 0x2234),
        (0x5D, 0x005D),
        (0x5E, 0x22A5),
        (0x5F, 0x005F),
        (0x60, 0xF8E5),
        (0x61, 0x03B1),
        (0x62, 0x03B2),
        (0x63, 0x03C7),
        (0x64, 0x03B4),
        (0x65, 0x03B5),
        (0x66, 0x03C6),
        (0x67, 0x03B3),
        (0x68, 0x03B7),
        (0x69, 0x03B9),
        (0x6A, 0x03D5),
        (0x6B, 0x03BA),
        (0x6C, 0x03BB),
        (0x6D, 0x03BC),
        (0x6E, 0x03BD),
        (0x6F, 0x03BF),
        (0x70, 0x03C0),
        (0x71, 0x03B8),
        (0x72, 0x03C1),
        (0x73, 0x03C3),
        (0x74, 0x03C4),
        (0x75, 0x03C5),
        (0x76, 0x03D6),
        (0x77, 0x03C9),
        (0x78, 0x03BE),
        (0x79, 0x03C8),
        (0x7A, 0x03B6),
        (0x7B, 0x007B),
        (0x7C, 0x007C),
        (0x7D, 0x007D),
        (0x7E, 0x223C),
        (0xA0, 0x20AC),
        (0xA1, 0x03D2),
        (0xA2, 0x2032),
        (0xA3, 0x2264),
        (0xA4, 0x2044),
        (0xA5, 0x221E),
        (0xA6, 0x0192),
        (0xA7, 0x2663),
        (0xA8, 0x2666),
        (0xA9, 0x2665),
        (0xAA, 0x2660),
        (0xAB, 0x2194),
        (0xAC, 0x2190),
        (0xAD, 0x2191),
        (0xAE, 0x2192),
        (0xAF, 0x2193),
        (0xB0, 0x00B0),
        (0xB1, 0x00B1),
        (0xB2, 0x2033),
        (0xB3, 0x2265),
        (0xB4, 0x00D7),
        (0xB5, 0x221D),
        (0xB6, 0x2202),
        (0xB7, 0x2022),
        (0xB8, 0x00F7),
        (0xB9, 0x2260),
        (0xBA, 0x2261),
        (0xBB, 0x2248),
        (0xBC, 0x2026),
        (0xBF, 0x21B5),
        (0xC0, 0x2135),
        (0xC5, 0x2295),
        (0xC6, 0x2205),
        (0xC7, 0x2229),
        (0xC8, 0x222A),
        (0xC9, 0x2283),
        (0xCA, 0x2287),
        (0xCB, 0x2284),
        (0xCC, 0x2282),
        (0xCD, 0x2286),
        (0xCE, 0x2208),
        (0xCF, 0x2209),
        (0xD0, 0x2220),
        (0xD1, 0x2207),
        (0xD5, 0x220F),
        (0xD6, 0x221A),
        (0xD7, 0x22C5),
        (0xD8, 0x00AC),
        (0xD9, 0x2227),
        (0xDA, 0x2228),
        (0xDB, 0x21D4),
        (0xDC, 0x21D0),
        (0xDD, 0x21D1),
        (0xDE, 0x21D2),
        (0xDF, 0x21D3),
        (0xE5, 0x2211),
        (0xF2, 0x222B),
        (0x30, 0x0030),
        (0x31, 0x0031),
        (0x32, 0x0032),
        (0x33, 0x0033),
        (0x34, 0x0034),
        (0x35, 0x0035),
        (0x36, 0x0036),
        (0x37, 0x0037),
        (0x38, 0x0038),
        (0x39, 0x0039),
    ];
    for &(code, u) in PAIRS {
        map[code as usize] = char::from_u32(u);
    }
}

/// ZapfDingbats built-in encoding (the used range).
fn zapf_encoding(map: &mut [Option<char>; 256]) {
    *map = [None; 256];
    map[0x20] = Some(' ');
    // 0x21..=0x7E maps to U+2701..U+275E in order, with a handful of gaps
    // that don't matter for text recovery.
    for code in 0x21u16..=0x7E {
        map[code as usize] = char::from_u32(0x2701 + (code as u32 - 0x21));
    }
    // 0x80..: ornaments; 0xA1..=0xEF maps to U+2761.. block.
    for code in 0xA1u16..=0xEF {
        map[code as usize] = char::from_u32(0x2761 + (code as u32 - 0xA1));
    }
}

fn build_simple_encoding(pdf: &Pdf, dict: &Dict, info: &mut FontInfo) {
    // Base table: the named encoding (direct or /BaseEncoding inside an
    // encoding dict); WinAnsi as the practical default.
    let enc_val = pdf.dict_get(dict, b"Encoding").ok().flatten();
    let mut base_name: Option<&[u8]> = match &enc_val {
        Some(Val::Name(n)) => Some(n),
        Some(Val::Dict(d)) => match dget(d, b"BaseEncoding") {
            Some(Val::Name(n)) => Some(n),
            _ => None,
        },
        _ => None,
    };
    // Symbol and ZapfDingbats use built-in encodings, not the Latin set.
    if base_name.is_none() {
        if info.symbol_font == SymbolFont::Symbol {
            base_name = Some(b"Symbol");
        } else if info.symbol_font == SymbolFont::ZapfDingbats {
            base_name = Some(b"ZapfDingbats");
        }
    }
    let high: &[u32; 128] = match base_name {
        Some(b"MacRomanEncoding") => &MAC_ROMAN_HIGH,
        Some(b"StandardEncoding") => &STANDARD_HIGH,
        _ => &WIN_ANSI_HIGH,
    };
    match base_name {
        Some(b"Symbol") => symbol_encoding(&mut info.to_unicode_simple),
        Some(b"ZapfDingbats") => zapf_encoding(&mut info.to_unicode_simple),
        _ => {
            for b in 0u16..256 {
                let c = if b < 128 {
                    char::from_u32(b as u32)
                } else {
                    char::from_u32(high[b as usize - 128])
                };
                info.to_unicode_simple[b as usize] = c.filter(|c| *c != '\0');
            }
        }
    }

    if let Ok(Some(Val::Dict(enc))) = pdf.dict_get(dict, b"Encoding") {
        if let Ok(Some(Val::Array(diffs))) = pdf.dict_get(&enc, b"Differences") {
            let mut code = 0usize;
            for o in &diffs {
                match pdf.resolve(o) {
                    Ok(Val::Num(v)) => code = v as usize,
                    Ok(Val::Name(n)) => {
                        if code < 256 {
                            info.to_unicode_simple[code] = glyph_to_unicode(n);
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
        }
    }
}

// ── ToUnicode CMap (bfchar / bfrange) ───────────────────────────────────────

fn parse_tounicode(data: &[u8], info: &mut FontInfo) {
    let text = String::from_utf8_lossy(data);

    let take_hex = |s: &str| -> Option<u32> { u32::from_str_radix(s, 16).ok() };
    let hex_to_string = |s: &str| -> String {
        // UTF-16BE code units in hex.
        let mut units = Vec::new();
        let mut i = 0;
        while i + 4 <= s.len() {
            if let Some(v) = take_hex(&s[i..i + 4]) {
                units.push(v as u16);
            }
            i += 4;
        }
        String::from_utf16_lossy(&units)
    };

    // bfchar sections: <src> <dst>
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("beginbfchar") {
        let Some(end) = rest[start..].find("endbfchar") else {
            break;
        };
        let body = &rest[start + 11..start + end];
        let toks: Vec<&str> = tokenize_hex(body);
        for pair in toks.chunks(2) {
            if let [src, dst] = pair {
                if let Some(code) = take_hex(src) {
                    let s = hex_to_string(dst);
                    if !s.is_empty() {
                        set_unicode(info, code, src.len(), s);
                    }
                }
            }
        }
        rest = &rest[start + end + 9..];
    }

    // bfrange sections: <lo> <hi> <dst>  |  <lo> <hi> [<dst> …]
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("beginbfrange") {
        let Some(end) = rest[start..].find("endbfrange") else {
            break;
        };
        let body = &rest[start + 12..start + end];
        parse_bfrange(body, info, &take_hex, &hex_to_string);
        rest = &rest[start + end + 10..];
    }
}

fn tokenize_hex(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        out.push(&rest[open + 1..open + close]);
        rest = &rest[open + close + 1..];
    }
    out
}

fn parse_bfrange(
    body: &str,
    info: &mut FontInfo,
    take_hex: &dyn Fn(&str) -> Option<u32>,
    hex_to_string: &dyn Fn(&str) -> String,
) {
    // Tokenize: hex strings plus [ … ] arrays of hex strings.
    let mut rest = body;
    while let Some(lo_open) = rest.find('<') {
        let Some(lo_close) = rest[lo_open..].find('>') else {
            break;
        };
        let lo_s = &rest[lo_open + 1..lo_open + lo_close];
        rest = &rest[lo_open + lo_close + 1..];

        let Some(hi_open) = rest.find('<') else { break };
        let Some(hi_close) = rest[hi_open..].find('>') else {
            break;
        };
        let hi_s = &rest[hi_open + 1..hi_open + hi_close];
        let code_len = lo_s.len();
        rest = &rest[hi_open + hi_close + 1..];

        let (Some(lo), Some(hi)) = (take_hex(lo_s), take_hex(hi_s)) else {
            break;
        };

        // Next token: '[' array or '<' hex.
        let next_bracket = rest.find('[');
        let next_hex = rest.find('<');
        match (next_bracket, next_hex) {
            (Some(b), Some(h)) if b < h => {
                let Some(close) = rest[b..].find(']') else {
                    break;
                };
                let arr = &rest[b + 1..b + close];
                for (i, tok) in tokenize_hex(arr).iter().enumerate() {
                    let s = hex_to_string(tok);
                    if !s.is_empty() {
                        set_unicode(info, lo + i as u32, code_len, s);
                    }
                }
                rest = &rest[b + close + 1..];
            }
            (_, Some(h)) => {
                let Some(close) = rest[h..].find('>') else {
                    break;
                };
                let dst = &rest[h + 1..h + close];
                let base = hex_to_string(dst);
                if let Some(base_first) = base.chars().next() {
                    let more: String = base.chars().skip(1).collect();
                    for code in lo..=hi.min(lo + 65535) {
                        let offset = code - lo;
                        if let Some(c) = char::from_u32(base_first as u32 + offset) {
                            let mut s = String::new();
                            s.push(c);
                            s.push_str(&more);
                            set_unicode(info, code, code_len, s);
                        }
                    }
                }
                rest = &rest[h + close + 1..];
            }
            _ => break,
        }
    }
}

fn set_unicode(info: &mut FontInfo, code: u32, hex_len: usize, s: String) {
    // 2-hex-digit source codes are single bytes (simple fonts).
    if hex_len <= 2 && code < 256 && s.chars().count() == 1 {
        info.to_unicode_simple[code as usize] = s.chars().next();
    }
    info.to_unicode.insert(code, s);
}

// ── Content interpreter ─────────────────────────────────────────────────────

pub(crate) struct RawItem {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub font_size: f64,
    pub is_bold: bool,
}

struct TextState {
    font: Option<std::rc::Rc<FontInfo>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    rise: f64,
    tm: Mat,
    tlm: Mat,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
            tm: IDENTITY,
            tlm: IDENTITY,
        }
    }
}

struct Interp<'a> {
    pdf: &'a Pdf<'a>,
    items: Vec<RawItem>,
    segments: Vec<Segment>,
    image_bboxes: Vec<(f64, f64, f64, f64)>, // user-space x0,y0,x1,y1
    /// Image XObject placements in paint order (dict + raw stream bytes).
    image_xobjects: Vec<(Dict<'a>, &'a [u8])>,
    /// Number of text-showing operators encountered (any font).
    text_ops: usize,
    /// A font with an unsupported predefined CMap showed text: the page
    /// cannot be decoded faithfully.
    unsupported_font: bool,
    /// Marked-content nesting depth, and the depth at which an
    /// /ActualText span opened (replacement text captured until the
    /// matching EMC).
    mc_depth: u32,
    actual_text: Option<ActualTextSpan>,
    /// Object numbers of OCGs OFF in the default configuration.
    hidden_ocgs: std::rc::Rc<FxHashSet<u32>>,
    /// Depth of the outermost hidden optional-content span, when inside.
    hidden_until: Option<u32>,
    ctm: Mat,
    ctm_stack: Vec<Mat>,
    ts: TextState,
    // path state for segment building
    path_start: Option<(f64, f64)>,
    path_cur: Option<(f64, f64)>,
    path_segments: Vec<(f64, f64, f64, f64)>, // raw line segments (user space, CTM applied)
    path_rects: Vec<(f64, f64, f64, f64)>,    // re ops: x, y, w, h (CTM applied, axis-aligned)
    seg_counter: usize,
    page_number: u32,
    depth: usize,
}

impl<'a> Interp<'a> {
    fn run(&mut self, content: &[u8], resources: Option<&Dict<'a>>) -> Result<()> {
        use super::content_lex::{Lexer, Operand};

        if self.depth > 6 {
            return Ok(()); // form recursion cap
        }
        let fonts = self.font_map(resources);
        let mut lex = Lexer::new(content);

        while let Some(op) = lex.next_op() {
            let n = |i: usize| -> Option<f64> {
                match lex.operands.get(i) {
                    Some(Operand::Num(v)) => Some(*v),
                    _ => None,
                }
            };
            match op {
                b"q" => self.ctm_stack.push(self.ctm),
                b"Q" => {
                    if let Some(m) = self.ctm_stack.pop() {
                        self.ctm = m;
                    }
                }
                b"cm" => {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
                        (n(0), n(1), n(2), n(3), n(4), n(5))
                    {
                        self.ctm = Mat { a, b, c, d, e, f }.mul(self.ctm);
                    }
                }
                b"BT" => {
                    self.ts.tm = IDENTITY;
                    self.ts.tlm = IDENTITY;
                }
                b"ET" => {}
                b"Tf" => {
                    if lex.operands.len() == 2 {
                        let name = lex.name_bytes(lex.operands[0]);
                        self.ts.font = fonts.get(name).cloned();
                        self.ts.size = n(1).unwrap_or(0.0);
                    }
                }
                b"Td" => {
                    if let (Some(tx), Some(ty)) = (n(0), n(1)) {
                        self.td(tx, ty);
                    }
                }
                b"TD" => {
                    if let (Some(tx), Some(ty)) = (n(0), n(1)) {
                        self.ts.leading = -ty;
                        self.td(tx, ty);
                    }
                }
                b"Tm" => {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
                        (n(0), n(1), n(2), n(3), n(4), n(5))
                    {
                        let m = Mat { a, b, c, d, e, f };
                        self.ts.tm = m;
                        self.ts.tlm = m;
                    }
                }
                b"T*" => self.next_line(),
                b"TL" => self.ts.leading = n(0).unwrap_or(0.0),
                b"Tc" => self.ts.char_spacing = n(0).unwrap_or(0.0),
                b"Tw" => self.ts.word_spacing = n(0).unwrap_or(0.0),
                b"Tz" => self.ts.h_scale = n(0).unwrap_or(100.0) / 100.0,
                b"Ts" => self.ts.rise = n(0).unwrap_or(0.0),
                b"Tj" => {
                    if let Some(o @ Operand::Str { .. }) = lex.operands.first().copied() {
                        self.show_text(lex.str_bytes(o));
                    }
                }
                b"'" => {
                    self.next_line();
                    if let Some(o @ Operand::Str { .. }) = lex.operands.first().copied() {
                        self.show_text(lex.str_bytes(o));
                    }
                }
                b"\"" => {
                    if lex.operands.len() == 3 {
                        self.ts.word_spacing = n(0).unwrap_or(0.0);
                        self.ts.char_spacing = n(1).unwrap_or(0.0);
                        self.next_line();
                        if let o @ Operand::Str { .. } = lex.operands[2] {
                            self.show_text(lex.str_bytes(o));
                        }
                    }
                }
                b"TJ" => {
                    for i in 0..lex.operands.len() {
                        match lex.operands[i] {
                            o @ Operand::Str { .. } => self.show_text(lex.str_bytes(o)),
                            Operand::Num(v) => self.kern(v),
                            _ => {}
                        }
                    }
                }
                // ── paths → segments ────────────────────────────────
                b"m" => {
                    if let (Some(x), Some(y)) = (n(0), n(1)) {
                        let p = self.ctm.apply(x, y);
                        self.path_start = Some(p);
                        self.path_cur = Some(p);
                    }
                }
                b"l" => {
                    if let (Some(x), Some(y)) = (n(0), n(1)) {
                        let p = self.ctm.apply(x, y);
                        if let Some(c) = self.path_cur {
                            self.path_segments.push((c.0, c.1, p.0, p.1));
                        }
                        self.path_cur = Some(p);
                    }
                }
                b"c" | b"v" | b"y" => {
                    let k = lex.operands.len();
                    if k >= 2 {
                        if let (Some(x), Some(y)) = (n(k - 2), n(k - 1)) {
                            self.path_cur = Some(self.ctm.apply(x, y));
                        }
                    }
                }
                b"h" => {
                    if let (Some(c), Some(s)) = (self.path_cur, self.path_start) {
                        self.path_segments.push((c.0, c.1, s.0, s.1));
                        self.path_cur = Some(s);
                    }
                }
                b"re" => {
                    if let (Some(x), Some(y), Some(w), Some(h)) = (n(0), n(1), n(2), n(3)) {
                        let (p0, p1) = (self.ctm.apply(x, y), self.ctm.apply(x + w, y + h));
                        self.path_rects.push((
                            p0.0.min(p1.0),
                            p0.1.min(p1.1),
                            (p1.0 - p0.0).abs(),
                            (p1.1 - p0.1).abs(),
                        ));
                    }
                }
                b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" => self.flush_path(true),
                b"f" | b"F" | b"f*" => self.flush_path(false),
                b"n" => self.clear_path(),
                // ── XObjects: forms (recurse) + images (bbox) ───────
                b"Do" => {
                    if let Some(o @ Operand::Name { .. }) = lex.operands.first().copied() {
                        let name = lex.name_bytes(o).to_vec();
                        self.do_xobject(&name, resources);
                    }
                }
                b"BMC" => self.mc_depth += 1,
                b"BDC" => {
                    self.mc_depth += 1;
                    // /OC spans referencing an OCG that is OFF in the
                    // default configuration are invisible: suppress
                    // their content like a viewer would.
                    if self.hidden_until.is_none() && !self.hidden_ocgs.is_empty() {
                        if let [o1 @ Operand::Name { .. }, o2 @ Operand::Name { .. }] =
                            lex.operands.as_slice()
                        {
                            if lex.name_bytes(*o1) == b"OC"
                                && self.ocg_hidden(lex.name_bytes(*o2), resources)
                            {
                                self.hidden_until = Some(self.mc_depth);
                            }
                        }
                    }
                    // /ActualText in the property dict replaces whatever
                    // the enclosed operators draw (MuPDF honors this).
                    if self.actual_text.is_none() {
                        if let Some(o @ Operand::Dict { .. }) = lex.operands.last().copied() {
                            if let Some(s) = parse_actual_text(lex.dict_bytes(o)) {
                                self.actual_text = Some(ActualTextSpan {
                                    text: s,
                                    depth: self.mc_depth,
                                    geom: None,
                                });
                            }
                        }
                    }
                }
                b"EMC" => {
                    if self.hidden_until == Some(self.mc_depth) {
                        self.hidden_until = None;
                    }
                    if let Some(span) = &self.actual_text {
                        if span.depth == self.mc_depth {
                            let span = self.actual_text.take().unwrap();
                            self.emit_actual_text(span);
                        }
                    }
                    self.mc_depth = self.mc_depth.saturating_sub(1);
                }
                // Inline image (BI…ID…EI, payload skipped by the lexer):
                // record the placement so image regions and scanned-page
                // detection see it. The empty payload marker keeps the
                // bbox/xobject pairing aligned; extraction of these
                // regions falls back to rasterization.
                b"BI" => {
                    let (ax, ay) = self.ctm.apply(0.0, 0.0);
                    let (bx, by) = self.ctm.apply(1.0, 1.0);
                    self.image_bboxes
                        .push((ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)));
                    self.image_xobjects.push((Vec::new(), &[]));
                }
                _ => {}
            }
            lex.clear();
        }
        Ok(())
    }

    /// Is the named /Properties entry an OCG (or OCMD) that the default
    /// configuration turns OFF?
    fn ocg_hidden(&self, name: &[u8], resources: Option<&Dict<'a>>) -> bool {
        let Some(res) = resources else { return false };
        let Ok(Some(Val::Dict(props))) = self.pdf.dict_get(res, b"Properties") else {
            return false;
        };
        match dget(&props, name) {
            Some(Val::Ref(num)) => {
                if self.hidden_ocgs.contains(num) {
                    return true;
                }
                // OCMD: /OCGs holds the actual group refs.
                if let Ok(Val::Dict(d)) = self.pdf.object(*num) {
                    match dget(&d, b"OCGs") {
                        Some(Val::Ref(n)) => return self.hidden_ocgs.contains(n),
                        Some(Val::Array(a)) => {
                            return a
                                .iter()
                                .any(|v| matches!(v, Val::Ref(n) if self.hidden_ocgs.contains(n)))
                        }
                        _ => {}
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Emit the replacement text of a closed /ActualText span using the
    /// geometry its suppressed runs accumulated.
    fn emit_actual_text(&mut self, span: ActualTextSpan) {
        if span.text.trim().is_empty() {
            return;
        }
        let Some((x0, y0, x1, size_dev, bold)) = span.geom else {
            return;
        };
        self.items.push(RawItem {
            text: span.text,
            x: x0,
            y: y0 - 0.20 * size_dev,
            width: (x1 - x0).abs().max(0.01),
            height: 1.2 * size_dev,
            font_size: (size_dev as i32) as f64,
            is_bold: bold,
        });
    }

    fn td(&mut self, tx: f64, ty: f64) {
        self.ts.tlm = Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
        .mul(self.ts.tlm);
        self.ts.tm = self.ts.tlm;
    }

    fn font_map(&self, resources: Option<&Dict<'a>>) -> FxHashMap<Vec<u8>, std::rc::Rc<FontInfo>> {
        let mut map = FxHashMap::default();
        if let Some(res) = resources {
            if let Ok(Some(Val::Dict(fonts))) = self.pdf.dict_get(res, b"Font") {
                for (name, obj) in &fonts {
                    if let Ok(Val::Dict(fd)) = self.pdf.resolve(obj) {
                        map.insert(name.to_vec(), std::rc::Rc::new(build_font(self.pdf, &fd)));
                    }
                }
            }
        }
        map
    }

    fn next_line(&mut self) {
        let l = self.ts.leading;
        self.ts.tlm = Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: -l,
        }
        .mul(self.ts.tlm);
        self.ts.tm = self.ts.tlm;
    }

    fn kern(&mut self, amount: f64) {
        // TJ number: translate by -amount/1000 × size × h_scale in text space.
        let tx = -amount / 1000.0 * self.ts.size * self.ts.h_scale;
        self.ts.tm = Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: 0.0,
        }
        .mul(self.ts.tm);
    }

    fn show_text(&mut self, bytes: &[u8]) {
        self.text_ops += 1;
        if self.hidden_until.is_some() {
            // Inside an OFF optional-content span: invisible.
            return;
        }
        if self.ts.font.as_ref().is_some_and(|f| f.unsupported_cmap) {
            self.unsupported_font = true;
            return;
        }
        let Some(font) = self.ts.font.clone() else {
            return;
        };
        if self.ts.size == 0.0 {
            return;
        }
        let trm = Mat {
            a: self.ts.size * self.ts.h_scale,
            b: 0.0,
            c: 0.0,
            d: self.ts.size,
            e: 0.0,
            f: self.ts.rise,
        }
        .mul(self.ts.tm)
        .mul(self.ctm);

        let (x0, y0) = trm.apply(0.0, 0.0);
        let size_dev = self.ts.size * self.ts.tm.mul(self.ctm).y_scale();
        let mut text = String::new();
        let mut advance = 0.0f64; // text-space units (pre-scale)

        // (code-or-cid, is-space-byte, resolved unicode override)
        let codes: Vec<(u32, bool, Option<char>)> = if let Some(cjk) = &font.cjk {
            // Variable-length codes; unicode comes straight from the
            // ordering's CID->Unicode table.
            cjk.decode(bytes)
                .into_iter()
                .map(|(cid, uni)| (cid, false, uni))
                .collect()
        } else if font.two_byte {
            bytes
                .chunks(2)
                .map(|c| {
                    let v = if c.len() == 2 {
                        ((c[0] as u32) << 8) | c[1] as u32
                    } else {
                        c[0] as u32
                    };
                    (v, false, None)
                })
                .collect()
        } else {
            bytes.iter().map(|&b| (b as u32, b == 32, None)).collect()
        };

        for (code, is_space_byte, uni_override) in codes {
            let w = if font.two_byte || font.cjk.is_some() {
                *font.cid_widths.get(&code).unwrap_or(&font.default_width)
            } else {
                let w = font.widths[code as usize & 0xff];
                if w != 0.0 {
                    w
                } else if font.default_width != 0.0 {
                    font.default_width
                } else {
                    500.0
                }
            };
            advance += (w / 1000.0 * self.ts.size
                + self.ts.char_spacing
                + if is_space_byte {
                    self.ts.word_spacing
                } else {
                    0.0
                })
                * self.ts.h_scale;

            if let Some(c) = uni_override {
                text.push(c);
            } else if font.cjk.is_some() {
                // Embedded/predefined CMap without a table hit: try
                // ToUnicode (keyed by CID), then the Adobe ordering.
                if let Some(s) = font.to_unicode.get(&code) {
                    text.push_str(s);
                } else if let Some(c) = font.adobe_ordering.as_ref().and_then(|m| m.lookup(code)) {
                    text.push(c);
                }
            } else if font.two_byte {
                if font.ucs2_codes {
                    // Codes are UCS-2 code units directly.
                    if let Some(c) = char::from_u32(code) {
                        text.push(c);
                    }
                } else if let Some(s) = font.to_unicode.get(&code) {
                    text.push_str(s);
                } else if let Some(c) = font.adobe_ordering.as_ref().and_then(|m| m.lookup(code)) {
                    text.push(c);
                } else if let Some(c) = char::from_u32(code) {
                    text.push(c);
                }
            } else if let Some(s) = font.to_unicode.get(&code) {
                text.push_str(s);
            } else if let Some(c) = font.to_unicode_simple[code as usize & 0xff] {
                text.push(c);
            }
        }

        // Advance Tm: horizontal writing moves across, vertical (WMode
        // 1) moves down the page.
        let (tx, ty) = if font.vertical {
            (0.0, -advance)
        } else {
            (advance, 0.0)
        };
        self.ts.tm = Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
        .mul(self.ts.tm);

        let (x1, _) = trm.apply(advance / self.ts.size.max(1e-9), 0.0);

        if let Some(span) = &mut self.actual_text {
            // Geometry only: the replacement text supersedes the glyphs.
            let bold = self.ts.font.as_ref().is_some_and(|f| f.is_bold);
            match &mut span.geom {
                Some((gx0, _gy0, gx1, gsize, _)) => {
                    *gx0 = gx0.min(x0.min(x1));
                    *gx1 = gx1.max(x0.max(x1));
                    *gsize = gsize.max(size_dev);
                }
                geom @ None => *geom = Some((x0.min(x1), y0, x0.max(x1), size_dev, bold)),
            }
            return;
        }

        if text.trim().is_empty() {
            return;
        }

        let width_dev = (x1 - x0).abs().max(0.01);
        self.items.push(RawItem {
            text,
            x: x0.min(x1),
            y: y0 - 0.20 * size_dev, // approximate descent below baseline
            width: width_dev,
            height: 1.2 * size_dev,
            // MuPDF's stext JSON int-truncates sizes; heading detection
            // clusters by size, so match that quantization.
            font_size: (size_dev as i32) as f64,
            is_bold: font.is_bold,
        });
    }

    fn clear_path(&mut self) {
        self.path_segments.clear();
        self.path_rects.clear();
        self.path_start = None;
        self.path_cur = None;
    }

    fn flush_path(&mut self, stroked: bool) {
        for (x, y, w, h) in std::mem::take(&mut self.path_rects) {
            if stroked {
                let id = format!("p{}-r{}", self.page_number, self.seg_counter);
                self.seg_counter += 1;
                super::extract::push_stroked_rect_edges_pub(&mut self.segments, &id, x, y, w, h);
            } else {
                let id = format!("p{}-fr{}", self.page_number, self.seg_counter);
                self.seg_counter += 1;
                if let Some(seg) = super::extract::thin_rect_to_segment_pub(id, x, y, w, h) {
                    self.segments.push(seg);
                }
            }
        }
        if stroked {
            for (x1, y1, x2, y2) in std::mem::take(&mut self.path_segments) {
                // Only axis-aligned-ish lines matter for table grids.
                if (x1 - x2).abs() < 0.8 || (y1 - y2).abs() < 0.8 {
                    let id = format!("p{}-l{}", self.page_number, self.seg_counter);
                    self.seg_counter += 1;
                    self.segments.push(Segment { id, x1, y1, x2, y2 });
                }
            }
        }
        self.clear_path();
    }

    fn do_xobject(&mut self, name: &[u8], resources: Option<&Dict<'a>>) {
        let Some(res) = resources else { return };
        let Ok(Some(Val::Dict(xobjects))) = self.pdf.dict_get(res, b"XObject") else {
            return;
        };
        let Some(obj) = dget(&xobjects, name) else {
            return;
        };
        let Ok(Val::Stream(sdict, raw)) = self.pdf.resolve(obj) else {
            return;
        };
        let subtype = dget(&sdict, b"Subtype")
            .and_then(|v| v.as_name())
            .unwrap_or(b"");

        if subtype == b"Image" {
            if self.hidden_until.is_some() {
                return;
            }
            // Unit square through the CTM.
            let (ax, ay) = self.ctm.apply(0.0, 0.0);
            let (bx, by) = self.ctm.apply(1.0, 1.0);
            self.image_bboxes
                .push((ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)));
            self.image_xobjects.push((sdict.clone(), raw));
            return;
        }

        if subtype == b"Form" {
            let Ok(data) = decode_stream(&sdict, raw, self.pdf) else {
                return;
            };
            let form_res = match self.pdf.dict_get(&sdict, b"Resources") {
                Ok(Some(Val::Dict(d))) => Some(d),
                _ => None,
            };
            let saved_ctm = self.ctm;
            if let Some(Val::Array(m)) = dget(&sdict, b"Matrix") {
                let v: Vec<f64> = m.iter().filter_map(|o| o.as_num()).collect();
                if v.len() == 6 {
                    self.ctm = Mat {
                        a: v[0],
                        b: v[1],
                        c: v[2],
                        d: v[3],
                        e: v[4],
                        f: v[5],
                    }
                    .mul(self.ctm);
                }
            }
            self.depth += 1;
            let _ = match &form_res {
                Some(fr) => self.run(&data, Some(fr)),
                None => self.run(&data, resources),
            };
            self.depth -= 1;
            self.ctm = saved_ctm;
        }
    }
}

// ── Page assembly ───────────────────────────────────────────────────────────

/// Object numbers of optional-content groups switched OFF in the
/// default viewer configuration (Catalog /OCProperties /D /OFF).
fn collect_hidden_ocgs(pdf: &Pdf, root: &Dict) -> std::rc::Rc<FxHashSet<u32>> {
    let mut set = FxHashSet::default();
    if let Ok(Some(Val::Dict(ocp))) = pdf.dict_get(root, b"OCProperties") {
        if let Ok(Some(Val::Dict(d))) = pdf.dict_get(&ocp, b"D") {
            if let Some(Val::Array(off)) = dget(&d, b"OFF") {
                for v in off {
                    if let Val::Ref(n) = v {
                        set.insert(*n);
                    }
                }
            }
        }
    }
    std::rc::Rc::new(set)
}

/// Base CTM for a page: MediaBox-origin normalization composed with the
/// /Rotate transform, plus the resulting (visual) page height. Rotation
/// maps content into an upright page of swapped dimensions, so the whole
/// downstream pipeline sees a normal page.
fn rotation_base(rotate: Option<f64>, mb: &[f64], mx0: f64, my0: f64) -> (Mat, f64) {
    let w = (mb[2] - mb[0]).abs();
    let h = (mb[3] - mb[1]).abs();
    let t = Mat {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: -mx0,
        f: -my0,
    };
    let r = ((rotate.unwrap_or(0.0) as i64 % 360) + 360) % 360;
    match r {
        90 => (
            // (x,y) → (y, w−x): 90° clockwise display; page dims swap.
            t.mul(Mat {
                a: 0.0,
                b: -1.0,
                c: 1.0,
                d: 0.0,
                e: 0.0,
                f: w,
            }),
            w,
        ),
        180 => (
            t.mul(Mat {
                a: -1.0,
                b: 0.0,
                c: 0.0,
                d: -1.0,
                e: w,
                f: h,
            }),
            h,
        ),
        270 => (
            t.mul(Mat {
                a: 0.0,
                b: 1.0,
                c: -1.0,
                d: 0.0,
                e: h,
                f: 0.0,
            }),
            w,
        ),
        _ => (t, h),
    }
}

/// Pull /ActualText out of a raw BDC property dict. Handles literal
/// strings (with escapes) and hex strings; UTF-16BE by BOM, else
/// PDFDocEncoding treated as latin1.
fn parse_actual_text(dict: &[u8]) -> Option<String> {
    let at = memchr::memmem::find(dict, b"/ActualText")?;
    let mut p = at + b"/ActualText".len();
    while p < dict.len() && dict[p].is_ascii_whitespace() {
        p += 1;
    }
    let bytes: Vec<u8> = match dict.get(p)? {
        b'(' => {
            let mut out = Vec::new();
            let mut depth = 1usize;
            p += 1;
            while p < dict.len() && depth > 0 {
                match dict[p] {
                    b'\\' => {
                        p += 1;
                        match dict.get(p)? {
                            b'n' => out.push(b'\n'),
                            b'r' => out.push(b'\r'),
                            b't' => out.push(b'\t'),
                            b'b' => out.push(8),
                            b'f' => out.push(12),
                            d @ b'0'..=b'7' => {
                                let mut v = (d - b'0') as u32;
                                for _ in 0..2 {
                                    match dict.get(p + 1) {
                                        Some(d2 @ b'0'..=b'7') => {
                                            v = v * 8 + (d2 - b'0') as u32;
                                            p += 1;
                                        }
                                        _ => break,
                                    }
                                }
                                out.push(v as u8);
                            }
                            &c => out.push(c),
                        }
                        p += 1;
                    }
                    b'(' => {
                        depth += 1;
                        out.push(b'(');
                        p += 1;
                    }
                    b')' => {
                        depth -= 1;
                        if depth > 0 {
                            out.push(b')');
                        }
                        p += 1;
                    }
                    c => {
                        out.push(c);
                        p += 1;
                    }
                }
            }
            out
        }
        b'<' => {
            let end = dict[p..].iter().position(|&b| b == b'>')? + p;
            let mut out = Vec::new();
            let mut hi: Option<u8> = None;
            for &b in &dict[p + 1..end] {
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
            out
        }
        _ => return None,
    };

    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        Some(String::from_utf16_lossy(&units))
    } else {
        Some(bytes.iter().map(|&b| b as char).collect())
    }
}

/// An open /ActualText marked-content span: replacement text plus the
/// geometry accumulated from the suppressed show-text operators.
struct ActualTextSpan {
    text: String,
    depth: u32,
    /// (x0, y0, x1, size_dev, is_bold) built up across runs.
    geom: Option<(f64, f64, f64, f64, bool)>,
}

/// Inheritable page-tree attributes.
#[derive(Clone, Default)]
struct Inherit<'a> {
    media: Option<Vec<f64>>,
    rotate: Option<f64>,
    resources: Option<Dict<'a>>,
}

fn walk_pages<'a>(
    pdf: &'a Pdf<'a>,
    node: &Dict<'a>,
    inh: &Inherit<'a>,
    out: &mut Vec<(Dict<'a>, Inherit<'a>)>,
    depth: usize,
) -> Result<()> {
    if depth > 32 {
        bail!("page tree too deep");
    }
    let mut inh = inh.clone();
    if let Some(Val::Array(a)) = pdf.dict_get(node, b"MediaBox")? {
        let v: Vec<f64> = a
            .iter()
            .filter_map(|o| pdf.resolve(o).ok().and_then(|v| v.as_num()))
            .collect();
        if v.len() == 4 {
            inh.media = Some(v);
        }
    }
    if let Some(v) = pdf.dict_get(node, b"Rotate")?.and_then(|v| v.as_num()) {
        inh.rotate = Some(v);
    }
    if let Some(Val::Dict(r)) = pdf.dict_get(node, b"Resources")? {
        inh.resources = Some(r);
    }

    if matches!(pdf.dict_get(node, b"Type")?, Some(Val::Name(b"Pages"))) {
        let Some(Val::Array(kids)) = pdf.dict_get(node, b"Kids")? else {
            bail!("Pages without Kids");
        };
        for kid in kids {
            let Val::Dict(kd) = pdf.resolve(&kid)? else {
                continue;
            };
            walk_pages(pdf, &kd, &inh, out, depth + 1)?;
        }
    } else {
        out.push((node.clone(), inh));
    }
    Ok(())
}

/// Extract all pages via the own object layer. Errors mean "use the
/// MuPDF fallback" (encryption, non-Flate filters, rotated pages,
/// zero-text documents, structural surprises).
pub fn extract_pages_fast(input: &[u8]) -> Result<Vec<PageContent>> {
    let pdf = Pdf::parse(input)?;

    // Page tree walk (Root → Pages → Kids), tracking inheritable attrs.
    let Some(Val::Dict(root)) = pdf.dict_get(&pdf.trailer, b"Root")? else {
        bail!("no Root");
    };
    let hidden_ocgs = collect_hidden_ocgs(&pdf, &root);
    let Some(Val::Dict(pages_root)) = pdf.dict_get(&root, b"Pages")? else {
        bail!("no Pages");
    };

    #[derive(Clone, Default)]
    struct Inherit<'a> {
        media: Option<Vec<f64>>,
        rotate: Option<f64>,
        resources: Option<Dict<'a>>,
    }

    fn walk<'a>(
        pdf: &'a Pdf<'a>,
        node: &Dict<'a>,
        inh: &Inherit<'a>,
        out: &mut Vec<(Dict<'a>, Inherit<'a>)>,
        depth: usize,
    ) -> Result<()> {
        if depth > 32 {
            bail!("page tree too deep");
        }
        let mut inh = inh.clone();
        if let Some(Val::Array(a)) = pdf.dict_get(node, b"MediaBox")? {
            let v: Vec<f64> = a
                .iter()
                .filter_map(|o| pdf.resolve(o).ok().and_then(|v| v.as_num()))
                .collect();
            if v.len() == 4 {
                inh.media = Some(v);
            }
        }
        if let Some(v) = pdf.dict_get(node, b"Rotate")?.and_then(|v| v.as_num()) {
            inh.rotate = Some(v);
        }
        if let Some(Val::Dict(r)) = pdf.dict_get(node, b"Resources")? {
            inh.resources = Some(r);
        }

        if matches!(pdf.dict_get(node, b"Type")?, Some(Val::Name(b"Pages"))) {
            let Some(Val::Array(kids)) = pdf.dict_get(node, b"Kids")? else {
                bail!("Pages without Kids");
            };
            for kid in kids {
                let Val::Dict(kd) = pdf.resolve(&kid)? else {
                    continue;
                };
                walk(pdf, &kd, &inh, out, depth + 1)?;
            }
        } else {
            out.push((node.clone(), inh));
        }
        Ok(())
    }

    let mut page_dicts = Vec::new();
    walk(&pdf, &pages_root, &Inherit::default(), &mut page_dicts, 0)?;

    let mut out: Vec<PageContent> = Vec::with_capacity(page_dicts.len());
    let mut any_text = false;
    let mut any_text_ops = false;
    let mut content_buf: Vec<u8> = Vec::new();

    for (idx, (page, inh)) in page_dicts.iter().enumerate() {
        let page_no = (idx + 1) as u32;

        let mb = inh.media.as_ref().ok_or_else(|| anyhow!("no MediaBox"))?;
        let (mx0, my0) = (mb[0].min(mb[2]), mb[1].min(mb[3]));
        let (base, page_height) = rotation_base(inh.rotate, mb, mx0, my0);

        // Concatenate content streams.
        content_buf.clear();
        match pdf.dict_get(page, b"Contents")? {
            Some(Val::Stream(d, raw)) => {
                content_buf.extend_from_slice(&decode_stream(&d, raw, &pdf)?);
            }
            Some(Val::Array(items)) => {
                for it in items {
                    if let Val::Stream(d, raw) = pdf.resolve(&it)? {
                        content_buf.extend_from_slice(&decode_stream(&d, raw, &pdf)?);
                        content_buf.push(b'\n');
                    }
                }
            }
            _ => {}
        }

        let mut interp = Interp {
            pdf: &pdf,
            items: Vec::new(),
            segments: Vec::new(),
            image_bboxes: Vec::new(),
            image_xobjects: Vec::new(),
            text_ops: 0,
            unsupported_font: false,
            mc_depth: 0,
            actual_text: None,
            hidden_ocgs: hidden_ocgs.clone(),
            hidden_until: None,
            ctm: base,
            ctm_stack: Vec::new(),
            ts: TextState::default(),
            path_start: None,
            path_cur: None,
            path_segments: Vec::new(),
            path_rects: Vec::new(),
            seg_counter: 0,
            page_number: page_no,
            depth: 0,
        };
        interp.run(&content_buf, inh.resources.as_ref())?;

        if !interp.items.is_empty() {
            any_text = true;
        }
        if interp.text_ops > 0 {
            any_text_ops = true;
        }
        if interp.unsupported_font {
            bail!("unsupported predefined CMap (CJK encoding tables)");
        }

        // Text boxes through the shared merge pipeline (items are already
        // in bottom-left user space).
        let raws: Vec<super::extract::RawTextItemPub> = interp
            .items
            .into_iter()
            .map(|i| super::extract::RawTextItemPub {
                text: i.text,
                x: i.x,
                y: i.y,
                width: i.width,
                height: i.height,
                font_size: i.font_size,
                is_bold: i.is_bold,
            })
            .collect();
        let text_boxes = super::extract::finish_text_boxes_pub(raws, page_no)?;

        // Image regions: convert user-space bbox to the device-space
        // convention image_regions expects (y down, int truncation).
        let bboxes: Vec<(f32, f32, f32, f32)> = interp
            .image_bboxes
            .iter()
            .map(|&(x0, y0, x1, y1)| {
                (
                    x0 as f32,
                    (page_height - y1) as f32,
                    x1 as f32,
                    (page_height - y0) as f32,
                )
            })
            .collect();
        let images = super::extract::image_regions_from_bboxes_pub(&bboxes, page_no, page_height);

        out.push(PageContent {
            page_number: page_no,
            text_boxes,
            segments: interp.segments,
            images,
        });
    }

    // Text operators that produced nothing = an encoding we failed to
    // decode: defer to the fallback. No text operators at all = a scanned
    // document: image placeholders are the right output, same as MuPDF.
    if !any_text && any_text_ops && !out.is_empty() {
        bail!("text ops decoded to nothing (unsupported encodings)");
    }

    Ok(out)
}
/// Image placements for one page, in the same order and with the same
/// area filter as the ImageRegion ids assigned during extraction
/// ("p{page}-img{i}"), so a region id indexes directly into this list.
pub(crate) fn page_image_placements<'a>(
    pdf: &'a Pdf<'a>,
    page_number: u32,
) -> Result<Vec<(Dict<'a>, &'a [u8])>> {
    let Some(Val::Dict(root)) = pdf.dict_get(&pdf.trailer, b"Root")? else {
        bail!("no Root");
    };
    let hidden_ocgs = collect_hidden_ocgs(pdf, &root);
    let Some(Val::Dict(pages_root)) = pdf.dict_get(&root, b"Pages")? else {
        bail!("no Pages");
    };
    let mut page_dicts = Vec::new();
    walk_pages(pdf, &pages_root, &Inherit::default(), &mut page_dicts, 0)?;
    let Some((page, inh)) = page_dicts.into_iter().nth(page_number as usize - 1) else {
        bail!("page out of range");
    };

    let mb = inh.media.as_ref().ok_or_else(|| anyhow!("no MediaBox"))?;
    let (mx0, my0) = (mb[0].min(mb[2]), mb[1].min(mb[3]));
    let (base, page_height) = rotation_base(inh.rotate, mb, mx0, my0);

    let mut content_buf: Vec<u8> = Vec::new();
    match pdf.dict_get(&page, b"Contents")? {
        Some(Val::Stream(d, raw)) => {
            content_buf.extend_from_slice(&decode_stream(&d, raw, pdf)?);
        }
        Some(Val::Array(items)) => {
            for it in items {
                if let Val::Stream(d, raw) = pdf.resolve(&it)? {
                    content_buf.extend_from_slice(&decode_stream(&d, raw, pdf)?);
                    content_buf.push(b'\n');
                }
            }
        }
        _ => {}
    }

    let mut interp = Interp {
        pdf,
        items: Vec::new(),
        segments: Vec::new(),
        image_bboxes: Vec::new(),
        image_xobjects: Vec::new(),
        text_ops: 0,
        unsupported_font: false,
        mc_depth: 0,
        actual_text: None,
        hidden_ocgs: hidden_ocgs.clone(),
        hidden_until: None,
        ctm: base,
        ctm_stack: Vec::new(),
        ts: TextState::default(),
        path_start: None,
        path_cur: None,
        path_segments: Vec::new(),
        path_rects: Vec::new(),
        seg_counter: 0,
        page_number,
        depth: 0,
    };
    interp.run(&content_buf, inh.resources.as_ref())?;

    // Apply the same MIN_IMAGE_AREA filter (int-truncated device coords)
    // that assigned the region ids.
    let mut out = Vec::new();
    for (i, &(x0, y0, x1, y1)) in interp.image_bboxes.iter().enumerate() {
        let dev = (
            x0 as f32,
            (page_height - y1) as f32,
            x1 as f32,
            (page_height - y0) as f32,
        );
        let w = ((dev.2 - dev.0) as i32) as f64;
        let h = ((dev.3 - dev.1) as i32) as f64;
        if w * h < super::extract::MIN_IMAGE_AREA_PUB {
            continue;
        }
        out.push(interp.image_xobjects[i].clone());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Option<Vec<u8>> {
        let path = format!("../test/fixtures/pdfs/encrypted/{name}");
        std::fs::read(path).ok()
    }

    fn text_of(pages: &[PageContent]) -> String {
        pages
            .iter()
            .flat_map(|p| p.text_boxes.iter())
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Every empty-password encryption revision must decrypt to exactly
    /// the plaintext document's extraction. Fixtures generated with qpdf
    /// from our own t.pdf (see test/fixtures/pdfs/encrypted/).
    #[test]
    fn encrypted_variants_match_plaintext() {
        let Some(plain) = fixture("plain.pdf") else {
            eprintln!("Skipping: encrypted fixtures not found");
            return;
        };
        let expect = text_of(&extract_pages_fast(&plain).unwrap());
        assert!(!expect.is_empty());

        for name in ["rc4-40.pdf", "rc4-128.pdf", "aesv2.pdf", "aes256.pdf"] {
            let bytes = fixture(name).unwrap();
            let pages = extract_pages_fast(&bytes)
                .unwrap_or_else(|e| panic!("{name}: fast path failed: {e}"));
            assert_eq!(text_of(&pages), expect, "{name} extraction differs");
        }

        // Password routes share this test because MARKIT_PDF_PASSWORD is
        // process-global: user and owner passwords both decrypt, a wrong
        // password is refused.
        for name in ["pw-aes256.pdf", "pw-rc4-128.pdf"] {
            let bytes = fixture(name).unwrap();
            for pw in ["usr", "own"] {
                std::env::set_var("MARKIT_PDF_PASSWORD", pw);
                let pages =
                    extract_pages_fast(&bytes).unwrap_or_else(|e| panic!("{name}/{pw}: {e}"));
                assert_eq!(text_of(&pages), expect, "{name}/{pw}");
            }
            std::env::set_var("MARKIT_PDF_PASSWORD", "wrong");
            assert!(
                extract_pages_fast(&bytes).is_err(),
                "{name} accepted a wrong password"
            );
            std::env::remove_var("MARKIT_PDF_PASSWORD");
        }
    }

    /// Password-protected fixtures decrypt with either the user or the
    /// owner password and refuse a wrong one. Env-var scoped in a single
    /// Content inside an /OC span whose OCG is OFF in the default
    /// configuration is invisible and must be suppressed.
    #[test]
    fn hidden_ocg_layer_suppressed() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [7 0 R] /D << /OFF [7 0 R] >> >> >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> /Properties << /MC0 7 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 92 >> stream
BT /F1 12 Tf 72 720 Td (visible) Tj /OC /MC0 BDC ( hidden) Tj EMC ( also-visible) Tj ET
endstream endobj
7 0 obj << /Type /OCG /Name (Watermark) >> endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("ocg");
        let text = text_of(&pages);
        assert!(text.contains("visible"), "got: {text}");
        assert!(!text.contains("hidden"), "got: {text}");
        assert!(text.contains("also-visible"), "got: {text}");
    }

    /// /ActualText in a marked-content span replaces the drawn glyphs
    /// (tagged-PDF semantics, same as MuPDF).
    #[test]
    fn actual_text_replaces_glyphs() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 96 >> stream
BT /F1 12 Tf 72 720 Td /Span << /ActualText (correct) >> BDC (wrong) Tj EMC ( after) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("actualtext");
        let text = text_of(&pages);
        assert!(text.contains("correct"), "got: {text}");
        assert!(!text.contains("wrong"), "got: {text}");
        assert!(text.contains("after"), "got: {text}");
    }

    /// A predefined CJK CMap (GBK-EUC-H) decodes both the multi-byte
    /// hanzi codespace and 1-byte ASCII through Adobe's tables.
    #[test]
    fn predefined_cjk_cmap_decodes() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 6 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /GBK-EUC-H /DescendantFonts [5 0 R] >> endobj
5 0 obj << /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >> /DW 1000 >> endobj
6 0 obj << /Length 52 >> stream
BT /F1 12 Tf 72 720 Td <C4E3BAC3> Tj (Hi) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("cjk cmap");
        let text = text_of(&pages);
        assert!(text.contains("\u{4F60}\u{597D}"), "got: {text}");
        assert!(text.contains("Hi"), "got: {text}");
    }

    /// A wrong /Length must not corrupt the stream: the parser verifies
    /// the endstream delimiter and recovers by scanning.
    #[test]
    fn wrong_stream_length_recovers() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 9999 >> stream
BT /F1 12 Tf 72 720 Td (Recovered) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("length repair");
        assert!(text_of(&pages).contains("Recovered"));
    }

    /// A rotated page must still extract its text (geometry transformed,
    /// not rejected).
    #[test]
    fn rotated_page_extracts() {
        // Minimal uncompressed PDF, /Rotate 90.
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 44 >> stream
BT /F1 12 Tf 72 720 Td (Hello rotated) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        // No xref: exercises the repair scan too.
        let pages = extract_pages_fast(pdf).expect("rotated page");
        let text = text_of(&pages);
        assert!(text.contains("Hello rotated"), "got: {text}");
        // 90° rotation swaps visual dimensions: the text box must sit
        // within the rotated page's width (the original height).
        let tb = &pages[0].text_boxes[0];
        assert!(tb.bounds.left >= 0.0 && tb.bounds.right <= 792.0);
    }
}
