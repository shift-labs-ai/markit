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
use rustc_hash::FxHashMap;

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

    let is_type0 = matches!(g(b"Subtype"), Some(Val::Name(b"Type0")));

    if is_type0 {
        info.two_byte = true; // Identity-H / 2-byte CMaps (the practical case)
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
            // No Widths: standard-14 territory. Courier is fixed 600;
            // others approximated (refined only if the benchmark cares).
            let w = if info.size_hint_monospace {
                600.0
            } else {
                500.0
            };
            info.widths = [w; 256];
        }
        if !info.is_bold {
            info.is_bold = descriptor_bold(pdf, dict);
        }
        build_simple_encoding(pdf, dict, &mut info);
    }

    // ToUnicode overrides encoding-derived mappings.
    if let Some(Val::Stream(sd, raw)) = g(b"ToUnicode") {
        if let Ok(data) = decode_stream(&sd, raw, pdf) {
            parse_tounicode(&data, &mut info);
        }
    }

    info
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

/// Small AGL subset covering the glyph names Differences arrays use in
/// practice for Latin text.
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
    Some(match s {
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
        "hyphen" | "minus" => '-',
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
        "quoteleft" => '\u{2018}',
        "quoteright" => '\u{2019}',
        "quotedblleft" => '\u{201C}',
        "quotedblright" => '\u{201D}',
        "endash" => '\u{2013}',
        "emdash" => '\u{2014}',
        "bullet" => '\u{2022}',
        "ellipsis" => '\u{2026}',
        "fi" => '\u{FB01}',
        "fl" => '\u{FB02}',
        "degree" => '\u{00B0}',
        "copyright" => '\u{00A9}',
        "registered" => '\u{00AE}',
        "trademark" => '\u{2122}',
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

fn build_simple_encoding(pdf: &Pdf, dict: &Dict, info: &mut FontInfo) {
    // Default: treat as WinAnsi-flavoured Latin (covers Standard/WinAnsi
    // ASCII range; MacRoman divergence is handled only via Differences).
    for b in 0u16..256 {
        let c = if b < 128 {
            char::from_u32(b as u32)
        } else {
            char::from_u32(WIN_ANSI_HIGH[b as usize - 128])
        };
        info.to_unicode_simple[b as usize] = c.filter(|c| *c != '\0');
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
                _ => {}
            }
            lex.clear();
        }
        Ok(())
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

        let codes: Vec<(u32, bool)> = if font.two_byte {
            bytes
                .chunks(2)
                .map(|c| {
                    let v = if c.len() == 2 {
                        ((c[0] as u32) << 8) | c[1] as u32
                    } else {
                        c[0] as u32
                    };
                    (v, false)
                })
                .collect()
        } else {
            bytes.iter().map(|&b| (b as u32, b == 32)).collect()
        };

        for (code, is_space_byte) in codes {
            let w = if font.two_byte {
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

            if font.two_byte {
                if let Some(s) = font.to_unicode.get(&code) {
                    text.push_str(s);
                } else if let Some(c) = char::from_u32(code) {
                    text.push(c);
                }
            } else if let Some(s) = font.to_unicode.get(&code) {
                text.push_str(s);
            } else if let Some(c) = font.to_unicode_simple[code as usize & 0xff] {
                text.push(c);
            }
        }

        // Advance Tm.
        let tx = advance;
        self.ts.tm = Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: 0.0,
        }
        .mul(self.ts.tm);

        if text.trim().is_empty() {
            return;
        }

        let (x1, _) = trm.apply(advance / self.ts.size.max(1e-9), 0.0);
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
    let mut content_buf: Vec<u8> = Vec::new();

    for (idx, (page, inh)) in page_dicts.iter().enumerate() {
        let page_no = (idx + 1) as u32;

        if let Some(r) = inh.rotate {
            if r as i64 % 360 != 0 {
                bail!("rotated page");
            }
        }
        let mb = inh.media.as_ref().ok_or_else(|| anyhow!("no MediaBox"))?;
        let (mx0, my0) = (mb[0].min(mb[2]), mb[1].min(mb[3]));
        let page_height = (mb[3] - mb[1]).abs();

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
            // Normalize a non-zero MediaBox origin to (0,0).
            ctm: Mat {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: -mx0,
                f: -my0,
            },
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

    if !any_text && !out.is_empty() {
        bail!("no text extracted (scanned or unsupported encodings)");
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
    let page_height = (mb[3] - mb[1]).abs();

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
        ctm: Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: -mx0,
            f: -my0,
        },
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
