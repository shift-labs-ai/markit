//! ToUnicode CMap parsing (bfchar / bfrange sections) into a font's
//! unicode tables.

use super::FontInfo;

pub(crate) fn parse_tounicode(data: &[u8], info: &mut FontInfo) {
    let source = String::from_utf8_lossy(data);
    // PostScript '%' comments run to end-of-line; hex-looking examples
    // inside them are not mappings.
    let cleaned;
    let text: &str = if source.contains('%') {
        cleaned = source
            .lines()
            .map(|line| line.split_once('%').map_or(line, |(code, _)| code))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        &cleaned
    } else {
        &source
    };

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
    let mut rest = text;
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
    let mut rest = text;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_comments_do_not_create_phantom_mappings() {
        let cmap = b"1 beginbfchar
% example <41> <0058>
<42> <0059>
endbfchar";
        let mut font = FontInfo::default();
        parse_tounicode(cmap, &mut font);
        assert_eq!(font.to_unicode_simple[0x41], None);
        assert_eq!(font.to_unicode_simple[0x42], Some('Y'));
    }

    #[test]
    fn scalar_and_array_bfranges_decode() {
        let cmap = b"2 beginbfrange
<01> <02> <0041>
<10> <11> [<03B1> <03B2>]
endbfrange";
        let mut font = FontInfo::default();
        parse_tounicode(cmap, &mut font);
        assert_eq!(font.to_unicode_simple[1], Some('A'));
        assert_eq!(font.to_unicode_simple[2], Some('B'));
        assert_eq!(font.to_unicode_simple[0x10], Some('α'));
        assert_eq!(font.to_unicode_simple[0x11], Some('β'));
    }
}
