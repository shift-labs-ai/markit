//! The Adobe Glyph List (practical subset) and glyph-name to unicode
//! resolution, shared by the simple-font encodings and the embedded
//! font-program parsers.

pub(crate) fn glyph_to_unicode(name: &[u8]) -> Option<char> {
    let s = std::str::from_utf8(name).ok()?;
    // uniXXXX may contain multiple four-digit code units; this
    // char-returning API resolves the first. uXXXX[XX] is one 4–6 digit
    // scalar and must consume every digit (supplementary planes).
    if let Some(hex) = s.strip_prefix("uni") {
        if hex.len() >= 4 {
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
    // Variant suffix ("eacute.sc", "one.onum") → base glyph.
    if let Some(dot) = s.find('.') {
        return agl_lookup(&s[..dot]);
    }
    None
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
}
