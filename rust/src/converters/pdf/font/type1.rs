//! Type1 and CFF font program parsing for encoding recovery.
//!
//! When a font dict carries no ToUnicode and no /Encoding, the embedded
//! program still declares its own encoding: Type1 programs in the
//! cleartext header ("dup <code> /<name> put"), CFF programs in the
//! Encoding/Charset structures. Glyph names then map through the AGL.

use super::glyphlist::glyph_to_unicode;

/// Parse the /Encoding section of a Type1 font program (the cleartext
/// part before eexec). Returns code -> unicode where recoverable.
pub fn type1_code_to_unicode(program: &[u8]) -> Option<[u32; 256]> {
    // Only the cleartext header matters; eexec starts the encrypted part.
    let end = memchr::memmem::find(program, b"eexec").unwrap_or(program.len());
    let head = &program[..end];
    let enc_at = memchr::memmem::find(head, b"/Encoding")?;
    let body = &head[enc_at..];

    // StandardEncoding shorthand: nothing to learn beyond the default.
    if body.starts_with(b"/Encoding StandardEncoding") {
        return None;
    }

    let mut out = [0u32; 256];
    let mut mapped = false;
    let mut pos = 0usize;
    while let Some(at) = memchr::memmem::find(&body[pos..], b"dup ") {
        let mut p = pos + at + 4;
        // <code>
        let ds = p;
        while p < body.len() && body[p].is_ascii_digit() {
            p += 1;
        }
        let Ok(code) = std::str::from_utf8(&body[ds..p])
            .unwrap_or("x")
            .parse::<usize>()
        else {
            pos += at + 4;
            continue;
        };
        // whitespace then /name
        while p < body.len() && body[p].is_ascii_whitespace() {
            p += 1;
        }
        if body.get(p) != Some(&b'/') {
            pos += at + 4;
            continue;
        }
        p += 1;
        let ns = p;
        while p < body.len() && !body[p].is_ascii_whitespace() && body[p] != b'/' {
            p += 1;
        }
        if code < 256 {
            if let Some(c) = glyph_to_unicode(&body[ns..p]) {
                out[code] = c as u32;
                mapped = true;
            }
        }
        pos = p;
        // "readonly def" ends the encoding vector.
        if memchr::memmem::find(&body[pos..(pos + 64).min(body.len())], b" def").is_some()
            && memchr::memmem::find(&body[pos..(pos + 16).min(body.len())], b"dup ").is_none()
        {
            // keep scanning anyway; harmless
        }
    }
    mapped.then_some(out)
}

// ── CFF ─────────────────────────────────────────────────────────────────────

struct CffIndex {
    offsets: Vec<usize>,
    data_start: usize,
}

impl CffIndex {
    fn parse(d: &[u8], at: usize) -> Option<(CffIndex, usize)> {
        let count = u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?]) as usize;
        if count == 0 {
            return Some((
                CffIndex {
                    offsets: Vec::new(),
                    data_start: 0,
                },
                at + 2,
            ));
        }
        let off_size = *d.get(at + 2)? as usize;
        if !(1..=4).contains(&off_size) {
            return None;
        }
        let mut offsets = Vec::with_capacity(count + 1);
        let mut p = at + 3;
        for _ in 0..=count {
            let mut v = 0usize;
            for _ in 0..off_size {
                v = (v << 8) | *d.get(p)? as usize;
                p += 1;
            }
            offsets.push(v);
        }
        let data_start = p;
        let end = data_start + offsets.last()? - 1;
        if end > d.len() {
            return None;
        }
        Some((
            CffIndex {
                offsets,
                data_start,
            },
            end,
        ))
    }

    fn get<'a>(&self, d: &'a [u8], i: usize) -> Option<&'a [u8]> {
        let s = self.data_start + self.offsets.get(i)? - 1;
        let e = self.data_start + self.offsets.get(i + 1)? - 1;
        d.get(s..e)
    }

    fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
}

/// CFF standard strings 0..=390 are predefined; we need them only for
/// glyph-name recovery, so map via the SID -> name table subset that the
/// AGL can resolve (the .notdef/ordinary ASCII ranges line up).
fn std_string(sid: u16) -> Option<&'static str> {
    const STD: &[&str] = &[
        ".notdef",
        "space",
        "exclam",
        "quotedbl",
        "numbersign",
        "dollar",
        "percent",
        "ampersand",
        "quoteright",
        "parenleft",
        "parenright",
        "asterisk",
        "plus",
        "comma",
        "hyphen",
        "period",
        "slash",
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "colon",
        "semicolon",
        "less",
        "equal",
        "greater",
        "question",
        "at",
        "A",
        "B",
        "C",
        "D",
        "E",
        "F",
        "G",
        "H",
        "I",
        "J",
        "K",
        "L",
        "M",
        "N",
        "O",
        "P",
        "Q",
        "R",
        "S",
        "T",
        "U",
        "V",
        "W",
        "X",
        "Y",
        "Z",
        "bracketleft",
        "backslash",
        "bracketright",
        "asciicircum",
        "underscore",
        "quoteleft",
        "a",
        "b",
        "c",
        "d",
        "e",
        "f",
        "g",
        "h",
        "i",
        "j",
        "k",
        "l",
        "m",
        "n",
        "o",
        "p",
        "q",
        "r",
        "s",
        "t",
        "u",
        "v",
        "w",
        "x",
        "y",
        "z",
        "braceleft",
        "bar",
        "braceright",
        "asciitilde",
        "exclamdown",
        "cent",
        "sterling",
        "fraction",
        "yen",
        "florin",
        "section",
        "currency",
        "quotesingle",
        "quotedblleft",
        "guillemotleft",
        "guilsinglleft",
        "guilsinglright",
        "fi",
        "fl",
        "endash",
        "dagger",
        "daggerdbl",
        "periodcentered",
        "paragraph",
        "bullet",
        "quotesinglbase",
        "quotedblbase",
        "quotedblright",
        "guillemotright",
        "ellipsis",
        "perthousand",
        "questiondown",
        "grave",
        "acute",
        "circumflex",
        "tilde",
        "macron",
        "breve",
        "dotaccent",
        "dieresis",
        "ring",
        "cedilla",
        "hungarumlaut",
        "ogonek",
        "caron",
        "emdash",
        "AE",
        "ordfeminine",
        "Lslash",
        "Oslash",
        "OE",
        "ordmasculine",
        "ae",
        "dotlessi",
        "lslash",
        "oslash",
        "oe",
        "germandbls",
        "onesuperior",
        "logicalnot",
        "mu",
        "trademark",
        "Eth",
        "onehalf",
        "plusminus",
        "Thorn",
        "onequarter",
        "divide",
        "brokenbar",
        "degree",
        "thorn",
        "threequarters",
        "twosuperior",
        "registered",
        "minus",
        "eth",
        "multiply",
        "threesuperior",
        "copyright",
        "Aacute",
        "Acircumflex",
        "Adieresis",
        "Agrave",
        "Aring",
        "Atilde",
        "Ccedilla",
        "Eacute",
        "Ecircumflex",
        "Edieresis",
        "Egrave",
        "Iacute",
        "Icircumflex",
        "Idieresis",
        "Igrave",
        "Ntilde",
        "Oacute",
        "Ocircumflex",
        "Odieresis",
        "Ograve",
        "Otilde",
        "Scaron",
        "Uacute",
        "Ucircumflex",
        "Udieresis",
        "Ugrave",
        "Yacute",
        "Ydieresis",
        "Zcaron",
        "aacute",
        "acircumflex",
        "adieresis",
        "agrave",
        "aring",
        "atilde",
        "ccedilla",
        "eacute",
        "ecircumflex",
        "edieresis",
        "egrave",
        "iacute",
        "icircumflex",
        "idieresis",
        "igrave",
        "ntilde",
        "oacute",
        "ocircumflex",
        "odieresis",
        "ograve",
        "otilde",
        "scaron",
        "uacute",
        "ucircumflex",
        "udieresis",
        "ugrave",
        "yacute",
        "ydieresis",
        "zcaron",
    ];
    STD.get(sid as usize).copied()
}

/// Parse a CFF program's Encoding + Charset into code -> unicode.
pub fn cff_code_to_unicode(program: &[u8]) -> Option<[u32; 256]> {
    // Header: major, minor, hdrSize, offSize.
    let hdr_size = *program.get(2)? as usize;
    // Name INDEX, Top DICT INDEX, String INDEX follow.
    let (_names, p1) = CffIndex::parse(program, hdr_size)?;
    let (top_dicts, p2) = CffIndex::parse(program, p1)?;
    let (strings, _p3) = CffIndex::parse(program, p2)?;
    let top = top_dicts.get(program, 0)?;

    // Walk the Top DICT for charset (op 15), Encoding (op 16), CharStrings (op 17).
    let (mut charset_off, mut encoding_off, mut charstrings_off) = (0usize, 0usize, 0usize);
    let mut operands: Vec<f64> = Vec::new();
    let mut i = 0usize;
    while i < top.len() {
        let b0 = top[i] as usize;
        match b0 {
            32..=246 => {
                operands.push(b0 as f64 - 139.0);
                i += 1;
            }
            247..=250 => {
                let b1 = *top.get(i + 1)? as f64;
                operands.push((b0 as f64 - 247.0) * 256.0 + b1 + 108.0);
                i += 2;
            }
            251..=254 => {
                let b1 = *top.get(i + 1)? as f64;
                operands.push(-(b0 as f64 - 251.0) * 256.0 - b1 - 108.0);
                i += 2;
            }
            28 => {
                let v = i16::from_be_bytes([*top.get(i + 1)?, *top.get(i + 2)?]);
                operands.push(v as f64);
                i += 3;
            }
            29 => {
                let v = i32::from_be_bytes([
                    *top.get(i + 1)?,
                    *top.get(i + 2)?,
                    *top.get(i + 3)?,
                    *top.get(i + 4)?,
                ]);
                operands.push(v as f64);
                i += 5;
            }
            30 => {
                // real number: nibble-packed, terminated by 0xf
                i += 1;
                loop {
                    let b = *top.get(i)?;
                    i += 1;
                    if b & 0x0f == 0x0f || b >> 4 == 0x0f {
                        break;
                    }
                }
                operands.push(0.0);
            }
            _ => {
                // operator
                let op = if b0 == 12 {
                    i += 2;
                    1200 + *top.get(i - 1)? as usize
                } else {
                    i += 1;
                    b0
                };
                match op {
                    15 => charset_off = operands.last().copied().unwrap_or(0.0) as usize,
                    16 => encoding_off = operands.last().copied().unwrap_or(0.0) as usize,
                    17 => charstrings_off = operands.last().copied().unwrap_or(0.0) as usize,
                    _ => {}
                }
                operands.clear();
            }
        }
    }

    let (charstrings, _) = CffIndex::parse(program, charstrings_off)?;
    let nglyphs = charstrings.len();
    if nglyphs == 0 {
        return None;
    }

    // Charset: gid -> SID. Predefined 0 = ISOAdobe (identity-ish SIDs).
    let mut gid_sid: Vec<u16> = vec![0; nglyphs];
    match charset_off {
        0 => {
            for (gid, s) in gid_sid.iter_mut().enumerate() {
                *s = gid as u16;
            }
        }
        1 | 2 => return None, // expert charsets: not text fonts
        off => {
            let fmt = *program.get(off)?;
            let mut gid = 1usize; // gid 0 = .notdef
            match fmt {
                0 => {
                    let mut p = off + 1;
                    while gid < nglyphs {
                        gid_sid[gid] = u16::from_be_bytes([*program.get(p)?, *program.get(p + 1)?]);
                        p += 2;
                        gid += 1;
                    }
                }
                1 | 2 => {
                    let mut p = off + 1;
                    while gid < nglyphs {
                        let first = u16::from_be_bytes([*program.get(p)?, *program.get(p + 1)?]);
                        let n_left = if fmt == 1 {
                            *program.get(p + 2)? as usize
                        } else {
                            u16::from_be_bytes([*program.get(p + 2)?, *program.get(p + 3)?])
                                as usize
                        };
                        p += if fmt == 1 { 3 } else { 4 };
                        for k in 0..=n_left {
                            if gid >= nglyphs {
                                break;
                            }
                            gid_sid[gid] = first + k as u16;
                            gid += 1;
                        }
                    }
                }
                _ => return None,
            }
        }
    }

    let sid_char = |sid: u16| -> Option<char> {
        if let Some(name) = std_string(sid) {
            return glyph_to_unicode(name.as_bytes());
        }
        let custom = strings.get(program, sid as usize - 391)?;
        glyph_to_unicode(custom)
    };

    let mut out = [0u32; 256];
    let mut mapped = false;
    match encoding_off {
        0 => {
            // Standard encoding: code -> SID via the standard table; the
            // std_string identity makes code==SID workable for the ASCII set.
            for code in 32u16..127 {
                if let Some(c) = sid_char(code - 31) {
                    // SID 1 = space at code 32.
                    out[code as usize] = c as u32;
                    mapped = true;
                }
            }
        }
        1 => return None, // expert encoding
        off => {
            let fmt = *program.get(off)? & 0x7f;
            match fmt {
                0 => {
                    let ncodes = *program.get(off + 1)? as usize;
                    for k in 0..ncodes {
                        let code = *program.get(off + 2 + k)? as usize;
                        let gid = k + 1;
                        if gid < nglyphs {
                            if let Some(c) = sid_char(gid_sid[gid]) {
                                out[code] = c as u32;
                                mapped = true;
                            }
                        }
                    }
                }
                1 => {
                    let nranges = *program.get(off + 1)? as usize;
                    let mut gid = 1usize;
                    let mut p = off + 2;
                    for _ in 0..nranges {
                        let first = *program.get(p)? as usize;
                        let n_left = *program.get(p + 1)? as usize;
                        p += 2;
                        for k in 0..=n_left {
                            let code = first + k;
                            if code < 256 && gid < nglyphs {
                                if let Some(c) = sid_char(gid_sid[gid]) {
                                    out[code] = c as u32;
                                    mapped = true;
                                }
                            }
                            gid += 1;
                        }
                    }
                }
                _ => return None,
            }
        }
    }
    mapped.then_some(out)
}
