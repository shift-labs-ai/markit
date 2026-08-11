//! Marked-content property parsing for /ActualText and optional-content
//! membership dictionaries. Scans PDF lexical structure without allocation
//! so decoy names in strings, comments, arrays, or nested dictionaries cannot
//! be mistaken for direct property keys.

use rustc_hash::FxHashSet;

fn is_pdf_space(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}

fn is_pdf_delimiter(byte: u8) -> bool {
    is_pdf_space(byte)
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

fn dict_name_value_offset(dict: &[u8], key: &[u8]) -> Option<usize> {
    let mut pos = 0;
    let mut dict_depth = 0usize;
    let mut array_depth = 0usize;
    while pos < dict.len() {
        match dict[pos] {
            b'%' => {
                pos += 1;
                while pos < dict.len() && !matches!(dict[pos], b'\r' | b'\n') {
                    pos += 1;
                }
            }
            b'(' => {
                pos += 1;
                let mut depth = 1usize;
                while pos < dict.len() && depth > 0 {
                    match dict[pos] {
                        b'\\' => pos = (pos + 2).min(dict.len()),
                        b'(' => {
                            depth += 1;
                            pos += 1;
                        }
                        b')' => {
                            depth -= 1;
                            pos += 1;
                        }
                        _ => pos += 1,
                    }
                }
            }
            b'<' if dict.get(pos + 1) == Some(&b'<') => {
                dict_depth += 1;
                pos += 2;
            }
            b'>' if dict.get(pos + 1) == Some(&b'>') => {
                dict_depth = dict_depth.saturating_sub(1);
                pos += 2;
            }
            b'<' => {
                pos += 1;
                while pos < dict.len() && dict[pos] != b'>' {
                    pos += 1;
                }
                pos += usize::from(pos < dict.len());
            }
            b'[' => {
                array_depth += 1;
                pos += 1;
            }
            b']' => {
                array_depth = array_depth.saturating_sub(1);
                pos += 1;
            }
            b'/' => {
                let start = pos + 1;
                pos = start;
                while pos < dict.len() && !is_pdf_delimiter(dict[pos]) {
                    pos += 1;
                }
                if dict_depth == 1 && array_depth == 0 && &dict[start..pos] == key {
                    while pos < dict.len() {
                        if is_pdf_space(dict[pos]) {
                            pos += 1;
                        } else if dict[pos] == b'%' {
                            while pos < dict.len() && !matches!(dict[pos], b'\r' | b'\n') {
                                pos += 1;
                            }
                        } else {
                            break;
                        }
                    }
                    return (pos < dict.len()).then_some(pos);
                }
            }
            _ => pos += 1,
        }
    }
    None
}

fn parse_unsigned_at(bytes: &[u8], mut pos: usize) -> Option<(u32, usize)> {
    while pos < bytes.len() && is_pdf_space(bytes[pos]) {
        pos += 1;
    }
    let start = pos;
    let mut value = 0u32;
    while let Some(digit @ b'0'..=b'9') = bytes.get(pos).copied() {
        value = value.checked_mul(10)?.checked_add((digit - b'0') as u32)?;
        pos += 1;
    }
    (pos > start).then_some((value, pos))
}

pub(crate) fn inline_ocg_is_hidden(dict: &[u8], hidden: &FxHashSet<u32>) -> bool {
    let Some(mut pos) = dict_name_value_offset(dict, b"OCGs") else {
        return false;
    };
    if dict.get(pos) == Some(&b'[') {
        pos += 1;
    }
    while pos < dict.len() && dict[pos] != b']' {
        let Some((object, after_object)) = parse_unsigned_at(dict, pos) else {
            pos += 1;
            continue;
        };
        let Some((_, after_generation)) = parse_unsigned_at(dict, after_object) else {
            pos = after_object;
            continue;
        };
        let mut marker = after_generation;
        while marker < dict.len() && is_pdf_space(dict[marker]) {
            marker += 1;
        }
        if dict.get(marker) == Some(&b'R') && hidden.contains(&object) {
            return true;
        }
        pos = marker.saturating_add(1);
    }
    false
}

pub(crate) fn parse_actual_text(dict: &[u8]) -> Option<String> {
    let mut p = dict_name_value_offset(dict, b"ActualText")?;
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
                            b'\n' => {}
                            b'\r' => {
                                if dict.get(p + 1) == Some(&b'\n') {
                                    p += 1;
                                }
                            }
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
            if let Some(h) = hi {
                out.push(h << 4);
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

#[cfg(test)]
mod tests {
    use super::parse_actual_text;

    #[test]
    fn actual_text_key_ignores_strings_comments_and_name_prefixes() {
        assert_eq!(
            parse_actual_text(b"<< /Alt (/ActualText (fake)) % /ActualText (also fake)\n /ActualTextual (prefix) /ActualText (real) >>").as_deref(),
            Some("real")
        );
    }

    #[test]
    fn actual_text_odd_hex_digit_is_zero_padded() {
        assert_eq!(
            parse_actual_text(b"<< /ActualText <414> >>").as_deref(),
            Some("A@")
        );
    }

    #[test]
    fn actual_text_literal_line_continuations_emit_no_newline() {
        assert_eq!(
            parse_actual_text(b"<< /ActualText (A\\\nB\\\r\nC) >>").as_deref(),
            Some("ABC")
        );
    }

    #[test]
    fn actual_text_literal_octal_parentheses_do_not_confuse_key_scanner() {
        assert_eq!(
            parse_actual_text(b"<< /Alt (\\050decoy\\051) /ActualText (\\050real\\051) >>")
                .as_deref(),
            Some("(real)")
        );
    }
}
