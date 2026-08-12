//! The Adobe Glyph List (practical subset) and glyph-name to unicode
//! resolution, shared by the simple-font encodings and the embedded
//! font-program parsers.

/// Multi-character glyph-name resolution. Underscore-joined compounds
/// are Adobe's ligature naming (`T_h`, `f_f_i`); `uniXXXXYYYY` carries
/// several UTF-16 code units. Falls back to the single-char resolver.
pub(crate) fn glyph_to_unicode_multi(name: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(name).ok()?;
    // Ligature compound: every component must resolve.
    if s.contains('_') {
        let parts: Vec<&str> = s.split('_').collect();
        if parts.len() >= 2 && parts.iter().all(|p| !p.is_empty()) {
            let mut out = String::new();
            for part in parts {
                out.push(glyph_to_unicode(part.as_bytes())?);
            }
            return Some(out);
        }
    }
    // uni with multiple 4-digit code units.
    if let Some(hex) = s.strip_prefix("uni") {
        if hex.len() > 4 && hex.len() % 4 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let units: Vec<u16> = (0..hex.len() / 4)
                .filter_map(|i| u16::from_str_radix(&hex[i * 4..i * 4 + 4], 16).ok())
                .collect();
            let out = String::from_utf16_lossy(&units);
            if !out.is_empty() {
                return Some(out);
            }
        }
    }
    glyph_to_unicode(name).map(String::from)
}

pub(crate) fn glyph_to_unicode(name: &[u8]) -> Option<char> {
    let s = std::str::from_utf8(name).ok()?;
    // uniXXXX may contain multiple four-digit code units; this
    // char-returning API resolves the first. uXXXX[XX] is one 4–6 digit
    // scalar and must consume every digit (supplementary planes).
    // Non-hex tails fall through: "universal" and "uniondisplay" are
    // glyph names, not uni-escapes.
    if let Some(hex) = s.strip_prefix("uni") {
        if hex.len() >= 4 && hex.as_bytes()[..4].iter().all(u8::is_ascii_hexdigit) {
            return u32::from_str_radix(&hex[..4], 16)
                .ok()
                .and_then(char::from_u32);
        }
    }
    if let Some(hex) = s.strip_prefix('u') {
        if (4..=6).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
        }
    }
    if let Some(c) = agl_lookup(s) {
        return Some(c);
    }
    if let Some(c) = math_lookup(s) {
        return Some(c);
    }
    // Variant suffix ("eacute.sc", "one.onum", "divides.alt0") → base.
    if let Some(dot) = s.find('.') {
        let base = &s[..dot];
        return agl_lookup(base).or_else(|| math_lookup(base));
    }
    None
}

/// TeX / Computer Modern math glyph names (dvips, pdftex output):
/// symbols, relations, arrows, and the sized/segmented delimiter
/// variants (parenleftBig, bracelefttp…) that arXiv PDFs carry in
/// their /Differences arrays.
fn math_lookup(s: &str) -> Option<char> {
    if let Some(c) = math_exact(s) {
        return Some(c);
    }
    // Sized and segmented variants: strip trailing digits, a stray
    // trailing capital (parenlefttpA), then one size/part suffix, and
    // retry both tables ("parenleftBig" → "parenleft").
    let t = s.trim_end_matches(|c: char| c.is_ascii_digit());
    let t = t.strip_suffix('A').unwrap_or(t);
    // Stripped bases must be real multi-letter names: opaque Type3
    // CharProc keys ("D5", "a49") would otherwise strip down to a
    // single letter and hit the letter-maps-to-itself rule.
    for suffix in [
        "display", "text", "widest", "wider", "wide", "Bigg", "bigg", "Big", "big", "tp", "bt",
        "mid", "ex",
    ] {
        if let Some(base) = t.strip_suffix(suffix) {
            if base.len() >= 3 {
                if let Some(c) = math_exact(base).or_else(|| agl_lookup(base)) {
                    return Some(c);
                }
            }
        }
    }
    if t != s && t.len() >= 3 {
        if let Some(c) = math_exact(t).or_else(|| agl_lookup(t)) {
            return Some(c);
        }
    }
    // Letter families: script (Acal), double-struck (Rbbb), oldstyle
    // figures.
    if let Some(base) = s.strip_suffix("cal") {
        if let [c @ b'A'..=b'Z'] = base.as_bytes() {
            return script_capital(*c as char);
        }
    }
    if let Some(base) = s.strip_suffix("bbb") {
        if let [c @ b'A'..=b'Z'] = base.as_bytes() {
            return double_struck_capital(*c as char);
        }
    }
    if let Some(base) = s.strip_suffix("oldstyle") {
        return agl_lookup(base).filter(char::is_ascii_digit);
    }
    None
}

/// Mathematical script capitals: the BMP has six legacy exceptions.
fn script_capital(c: char) -> Option<char> {
    Some(match c {
        'B' => '\u{212C}',
        'E' => '\u{2130}',
        'F' => '\u{2131}',
        'H' => '\u{210B}',
        'I' => '\u{2110}',
        'L' => '\u{2112}',
        'M' => '\u{2133}',
        'R' => '\u{211B}',
        _ => char::from_u32(0x1D49C + (c as u32 - 'A' as u32))?,
    })
}

/// Mathematical double-struck capitals with the BMP exceptions.
fn double_struck_capital(c: char) -> Option<char> {
    Some(match c {
        'C' => '\u{2102}',
        'H' => '\u{210D}',
        'N' => '\u{2115}',
        'P' => '\u{2119}',
        'Q' => '\u{211A}',
        'R' => '\u{211D}',
        'Z' => '\u{2124}',
        _ => char::from_u32(0x1D538 + (c as u32 - 'A' as u32))?,
    })
}

fn math_exact(s: &str) -> Option<char> {
    Some(match s {
        // ── operators & binary relations ─────────────────────
        "asteriskmath" => '\u{2217}',
        "prime" => '\u{2032}',
        "negationslash" => '\u{0338}',
        "lscript" => '\u{2113}',
        "openbullet" => '\u{2218}',
        "dotmath" => '\u{22C5}',
        "similar" => '\u{223C}',
        "similarequal" => '\u{2243}',
        "congruent" => '\u{2245}',
        "equivasymptotic" => '\u{224D}',
        "nequal" => '\u{2260}',
        "notapproxequal" => '\u{2249}',
        "proportional" => '\u{221D}',
        "minusplus" => '\u{2213}',
        "divides" => '\u{2223}',
        "notbar" => '\u{2224}',
        "parallel" => '\u{2225}',
        "integerdivide" => '\u{00F7}',
        "ratio" => '\u{2236}',
        "because" => '\u{2235}',
        "therefore" => '\u{2234}',
        "defines" => '\u{225C}',
        "wreathproduct" => '\u{2240}',
        "fork" => '\u{22D4}',
        "tie" => '\u{2040}',
        "star" => '\u{22C6}',
        "diamondmath" => '\u{22C4}',
        // ── sets & logic ─────────────────────────────────
        "propersubset" => '\u{2282}',
        "reflexsubset" => '\u{2286}',
        "propersuperset" => '\u{2283}',
        "reflexsuperset" => '\u{2287}',
        "subsetsqequal" => '\u{2291}',
        "supersetsqequal" => '\u{2292}',
        "subsetdbl" => '\u{22D0}',
        "subsetnoteql" => '\u{2284}',
        "notsubseteql" => '\u{2288}',
        "subsetornotdbleql" => '\u{228A}',
        "notelement" => '\u{2209}',
        "owner" => '\u{220B}',
        "universal" => '\u{2200}',
        "existential" => '\u{2203}',
        "emptyset" | "emptysetstress" => '\u{2205}',
        "nabla" | "gradient" => '\u{2207}',
        "logicaland" => '\u{2227}',
        "logicalor" => '\u{2228}',
        "unionsq" => '\u{2294}',
        "intersectionsq" => '\u{2293}',
        "unionmulti" | "capplus" => '\u{228E}',
        "coproduct" => '\u{2210}',
        "contintegral" => '\u{222E}',
        // ── circled operators ───────────────────────────
        "circlemultiply" => '\u{2297}',
        "circleplus" => '\u{2295}',
        "circleminus" => '\u{2296}',
        "circledot" | "circleot" => '\u{2299}',
        "circledivide" => '\u{2298}',
        "circlering" => '\u{229A}',
        "circle" | "largecircle" => '\u{25CB}',
        "blackcircle" => '\u{25CF}',
        "circlecopyrt" => '\u{00A9}',
        // ── order relations ──────────────────────────────
        "lessmuch" => '\u{226A}',
        "greatermuch" => '\u{226B}',
        "lessorequalslant" => '\u{2A7D}',
        "greaterorequalslant" => '\u{2A7E}',
        "lessorsimilar" => '\u{2272}',
        "lessnotequal" => '\u{2268}',
        "lessdblequal" => '\u{2266}',
        "notlessequal" => '\u{2270}',
        "notgreaterorslnteql" => '\u{2271}',
        "precedes" => '\u{227A}',
        "follows" => '\u{227B}',
        "precedesequal" => '\u{2AAF}',
        "followsequal" => '\u{2AB0}',
        "precedesorcurly" => '\u{227C}',
        "perpendicular" => '\u{22A5}',
        "latticetop" => '\u{22A4}',
        "turnstileleft" => '\u{22A2}',
        "turnstileright" => '\u{22A3}',
        "forces" => '\u{22A9}',
        // ── arrows ──────────────────────────────────────
        "mapsto" | "mapstochar" => '\u{21A6}',
        "arrowdblright" => '\u{21D2}',
        "arrowdblleft" => '\u{21D0}',
        "arrowdblboth" => '\u{21D4}',
        "arrowdblup" => '\u{21D1}',
        "arrowdbldown" => '\u{21D3}',
        "arrowdblbothv" => '\u{21D5}',
        "arrowhookleft" => '\u{21A9}',
        "arrowhookright" => '\u{21AA}',
        "arrowbothv" => '\u{2195}',
        "arrownortheast" => '\u{2197}',
        "arrowsoutheast" => '\u{2198}',
        "arrownorthwest" => '\u{2196}',
        "arrowsouthwest" => '\u{2199}',
        "arrowlefttophalf" => '\u{21BC}',
        "arrowleftbothalf" => '\u{21BD}',
        "arrowrighttophalf" => '\u{21C0}',
        "arrowrightbothalf" => '\u{21C1}',
        "harpoonupright" => '\u{21C0}',
        "dblarrowheadright" => '\u{21A0}',
        "dblarrowright" => '\u{21C9}',
        "shortrightarrow" | "arrowaxisright" => '\u{2192}',
        "squiggleright" => '\u{21DD}',
        "leftrightline" => '\u{2194}',
        "hookrightchar" => '\u{21AA}',
        // ── shapes ──────────────────────────────────────
        "square" | "Box" => '\u{25A1}',
        "squaresolid" => '\u{25A0}',
        "squaresmallsolid" => '\u{25AA}',
        "squaremultiply" => '\u{22A0}',
        "triangle" => '\u{25B3}',
        "triangleinv" => '\u{25BD}',
        "triangleleft" => '\u{25C1}',
        "triangleright" => '\u{25B7}',
        "trianglesolid" => '\u{25B2}',
        "triangleleftequal" => '\u{22B4}',
        "whitediamond" => '\u{25C7}',
        "diamondplus" => '\u{27D0}',
        // ── delimiters (sized variants resolve via suffix strip) ──
        "bardbl" | "bardblex" | "vextenddouble" => '\u{2016}',
        "vextendsingle" | "arrowvertex" | "barex" => '|',
        "angbracketleft" | "angleleft" => '\u{27E8}',
        "angbracketright" | "angleright" => '\u{27E9}',
        "floorleft" => '\u{230A}',
        "floorright" => '\u{230B}',
        "ceilingleft" => '\u{2308}',
        "ceilingright" => '\u{2309}',
        "llbracket" | "dblbracketleft" => '\u{27E6}',
        "rrbracket" | "dblbracketright" => '\u{27E7}',
        "braceex" => '\u{23AA}',
        // ── letterlike ──────────────────────────────────
        "aleph" => '\u{2135}',
        "daleth" => '\u{2138}',
        "weierstrass" => '\u{2118}',
        "Rfractur" => '\u{211C}',
        "Ifractur" => '\u{2111}',
        "Digamma" => '\u{03DC}',
        "planckover2pi" | "planckover2pi1" => '\u{210F}',
        "dotlessj" => '\u{0237}',
        // ── Greek variants (TeX naming) ─────────────────────
        "epsilon1" => '\u{03B5}',
        "phi1" => '\u{03C6}',
        "theta1" => '\u{03D1}',
        "rho1" => '\u{03F1}',
        "pi1" => '\u{03D6}',
        // ── music & misc ────────────────────────────────
        "sharp" | "musicsharpsign" => '\u{266F}',
        "flat" => '\u{266D}',
        "natural" => '\u{266E}',
        "angle" => '\u{2220}',
        "visualspace" => ' ',
        // ── wide accents: spacing forms so the overlay composer can
        // attach them to their base letter ────────────────────
        "tildewide" => '\u{02DC}',
        "hatwide" | "hat" => '\u{02C6}',
        "tildecomb" => '\u{0303}',
        "circumflexcmb" => '\u{0302}',
        "vector" | "vec" => '\u{20D7}',
        // ── radical: every segment reads as the sign ────────────
        "radical" | "radicalvertex" => '\u{221A}',
        _ => return None,
    })
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
        "mu" => '\u{03BC}',
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
        "Abreve" => '\u{0102}',
        "dcaron" => '\u{010F}',
        "Dcaron" => '\u{010E}',
        "tcaron" => '\u{0165}',
        "Tcaron" => '\u{0164}',
        "lcaron" => '\u{013E}',
        "Lcaron" => '\u{013D}',
        "Ecaron" => '\u{011A}',
        "Ccaron" => '\u{010C}',
        "ncaron" => '\u{0148}',
        "Ncaron" => '\u{0147}',
        "racute" => '\u{0155}',
        "Racute" => '\u{0154}',
        "lacute" => '\u{013A}',
        "Lacute" => '\u{0139}',
        "Zacute" => '\u{0179}',
        "uring" => '\u{016F}',
        "Uring" => '\u{016E}',
        "ohungarumlaut" => '\u{0151}',
        "Ohungarumlaut" => '\u{0150}',
        "uhungarumlaut" => '\u{0171}',
        "scedilla" => '\u{015F}',
        "Scedilla" => '\u{015E}',
        "tcedilla" => '\u{0163}',
        "gbreve" => '\u{011F}',
        "Gbreve" => '\u{011E}',
        "ij" => '\u{0133}',
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
        "Ydieresis" => '\u{0178}',
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplementary_u_name_uses_all_hex_digits() {
        assert_eq!(glyph_to_unicode(b"u1F600"), Some('😀'));
    }

    #[test]
    fn mu_and_micro_are_distinct_agl_names() {
        assert_eq!(glyph_to_unicode(b"mu"), Some('μ'));
        assert_eq!(glyph_to_unicode(b"micro"), Some('µ'));
    }

    #[test]
    fn variant_suffix_falls_back_to_base_name() {
        assert_eq!(glyph_to_unicode(b"eacute.sc"), Some('é'));
        assert_eq!(glyph_to_unicode(b"unknown"), None);
    }

    #[test]
    fn underscore_compounds_resolve_as_ligatures() {
        assert_eq!(glyph_to_unicode_multi(b"T_h").as_deref(), Some("Th"));
        assert_eq!(glyph_to_unicode_multi(b"f_f_i").as_deref(), Some("ffi"));
        assert_eq!(glyph_to_unicode_multi(b"f_i").as_deref(), Some("fi"));
        assert_eq!(glyph_to_unicode_multi(b"f_b").as_deref(), Some("fb"));
        // Unresolvable component: no partial output.
        assert_eq!(glyph_to_unicode_multi(b"T_qq"), None);
        // Multi-unit uni name.
        assert_eq!(
            glyph_to_unicode_multi(b"uni00540068").as_deref(),
            Some("Th")
        );
        // Plain names still resolve through the single-char path.
        assert_eq!(glyph_to_unicode_multi(b"eacute").as_deref(), Some("\u{e9}"));
    }
}
