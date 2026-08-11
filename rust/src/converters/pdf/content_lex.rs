//! Zero-copy streaming lexer for PDF content streams.
//!
//! No operation list is ever materialized:
//! no per-op Strings — operands accumulate in reused buffers and each
//! operator dispatches through a callback. String/hex operands are
//! unescaped into a scratch arena that resets per operator.

/// One operand on the stack. Strings and names index into the scratch
/// arena / source buffer to keep the steady state allocation-free.
#[derive(Clone, Copy, Debug)]
pub enum Operand {
    Num(f64),
    /// Range into the scratch arena (unescaped string bytes).
    Str {
        start: usize,
        len: usize,
    },
    /// Range into the source content (name bytes, after the slash).
    Name {
        start: usize,
        len: usize,
    },
    /// Range into the source content: a raw << … >> dictionary literal.
    Dict {
        start: usize,
        len: usize,
    },
    /// Marker: an array literal opened at this operand index.
    ArrStart,
    Other,
}

pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
    pub operands: Vec<Operand>,
    pub scratch: Vec<u8>,
}

use super::own_pdf::{is_delim, is_ws};

fn is_known_content_operator(op: &[u8]) -> bool {
    matches!(
        op,
        b"q" | b"Q"
            | b"cm"
            | b"w"
            | b"J"
            | b"j"
            | b"M"
            | b"d"
            | b"ri"
            | b"i"
            | b"gs"
            | b"m"
            | b"l"
            | b"c"
            | b"v"
            | b"y"
            | b"h"
            | b"re"
            | b"S"
            | b"s"
            | b"f"
            | b"F"
            | b"f*"
            | b"B"
            | b"B*"
            | b"b"
            | b"b*"
            | b"n"
            | b"W"
            | b"W*"
            | b"BT"
            | b"ET"
            | b"Tc"
            | b"Tw"
            | b"Tz"
            | b"TL"
            | b"Tf"
            | b"Tr"
            | b"Ts"
            | b"Td"
            | b"TD"
            | b"Tm"
            | b"T*"
            | b"Tj"
            | b"TJ"
            | b"'"
            | b"\""
            | b"d0"
            | b"d1"
            | b"CS"
            | b"cs"
            | b"SC"
            | b"SCN"
            | b"sc"
            | b"scn"
            | b"G"
            | b"g"
            | b"RG"
            | b"rg"
            | b"K"
            | b"k"
            | b"sh"
            | b"BI"
            | b"ID"
            | b"EI"
            | b"Do"
            | b"MP"
            | b"DP"
            | b"BMC"
            | b"BDC"
            | b"EMC"
            | b"BX"
            | b"EX"
    )
}

fn plausible_post_inline_syntax(mut data: &[u8]) -> bool {
    if data.iter().all(|byte| is_ws(*byte)) {
        return true;
    }
    // Validate the first post-EI operator. This rejects EI-looking bytes in
    // compressed payloads without decoding or allocating. Limit lookahead so
    // malformed streams remain strictly linear-time.
    if data.len() > 256 {
        data = &data[..256];
    }
    let mut pos = 0usize;
    while pos < data.len() {
        while pos < data.len() && is_ws(data[pos]) {
            pos += 1;
        }
        if pos == data.len() {
            return true;
        }
        match data[pos] {
            b'%' => {
                while pos < data.len() && !matches!(data[pos], b'\r' | b'\n') {
                    pos += 1;
                }
            }
            b'/' => {
                pos += 1;
                while pos < data.len() && !is_ws(data[pos]) && !is_delim(data[pos]) {
                    pos += 1;
                }
            }
            b'(' => {
                pos += 1;
                let mut depth = 1usize;
                while pos < data.len() && depth > 0 {
                    match data[pos] {
                        b'\\' => pos = (pos + 2).min(data.len()),
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
            b'<' => {
                pos += 1;
                while pos < data.len() && data[pos] != b'>' {
                    pos += 1;
                }
                pos += usize::from(pos < data.len());
            }
            b'[' | b']' => pos += 1,
            b'\'' | b'"' => return true,
            _ if is_delim(data[pos]) => pos += 1,
            _ => {
                let start = pos;
                while pos < data.len() && !is_ws(data[pos]) && !is_delim(data[pos]) {
                    pos += 1;
                }
                let token = &data[start..pos];
                if token.first().is_some_and(u8::is_ascii_alphabetic) {
                    return is_known_content_operator(token);
                }
                if !token
                    .iter()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'+' | b'-' | b'.'))
                {
                    return false;
                }
            }
        }
    }
    false
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Lexer {
            data,
            pos: 0,
            operands: Vec::with_capacity(16),
            scratch: Vec::with_capacity(256),
        }
    }

    pub fn name_bytes(&self, op: Operand) -> &'a [u8] {
        match op {
            Operand::Name { start, len } => &self.data[start..start + len],
            _ => &[],
        }
    }

    pub fn dict_bytes(&self, op: Operand) -> &'a [u8] {
        match op {
            Operand::Dict { start, len } => &self.data[start..start + len],
            _ => &[],
        }
    }

    pub fn str_bytes(&self, op: Operand) -> &[u8] {
        match op {
            Operand::Str { start, len } => &self.scratch[start..start + len],
            _ => &[],
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while self.pos < self.data.len() && !matches!(self.data[self.pos], b'\n' | b'\r') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Lex until the next operator; returns its bytes, or None at EOF.
    /// Operands accumulate in `self.operands` (cleared by the caller
    /// after dispatch, which also resets the scratch arena).
    pub fn next_op(&mut self) -> Option<&'a [u8]> {
        loop {
            self.skip_ws();
            if self.pos >= self.data.len() {
                return None;
            }
            let b = self.data[self.pos];
            match b {
                b'0'..=b'9' | b'+' | b'-' | b'.' => {
                    let v = self.lex_number();
                    self.operands.push(Operand::Num(v));
                }
                b'/' => {
                    self.pos += 1;
                    let start = self.pos;
                    while self.pos < self.data.len()
                        && !is_ws(self.data[self.pos])
                        && !is_delim(self.data[self.pos])
                    {
                        self.pos += 1;
                    }
                    self.operands.push(Operand::Name {
                        start,
                        len: self.pos - start,
                    });
                }
                b'(' => self.lex_string(),
                b'<' => {
                    if self.data.get(self.pos + 1) == Some(&b'<') {
                        // skip_dict records the span as Operand::Dict.
                        self.skip_dict();
                    } else {
                        self.lex_hex_string();
                    }
                }
                b'[' => {
                    self.pos += 1;
                    self.operands.push(Operand::ArrStart);
                }
                b']' => {
                    self.pos += 1;
                    // Array contents stay on the stack after the marker.
                }
                b'{' | b'}' | b')' | b'>' => {
                    self.pos += 1; // stray delimiter: skip defensively
                }
                _ => {
                    // Operator or keyword.
                    let start = self.pos;
                    while self.pos < self.data.len()
                        && !is_ws(self.data[self.pos])
                        && !is_delim(self.data[self.pos])
                    {
                        self.pos += 1;
                    }
                    let tok = &self.data[start..self.pos];
                    match tok {
                        b"true" | b"false" | b"null" => {
                            self.operands.push(Operand::Other);
                        }
                        b"BI" => {
                            self.skip_inline_image();
                            // Surface the inline image as an operator so
                            // interpreters can record its placement.
                            return Some(b"BI");
                        }
                        _ => return Some(tok),
                    }
                }
            }
        }
    }

    /// Reset operand + scratch state after an operator dispatch.
    pub fn clear(&mut self) {
        self.operands.clear();
        self.scratch.clear();
    }

    fn lex_number(&mut self) -> f64 {
        let start = self.pos;
        if matches!(self.data[self.pos], b'+' | b'-') {
            self.pos += 1;
        }
        while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.data.len() && self.data[self.pos] == b'.' {
            self.pos += 1;
            while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        let s = &self.data[start..self.pos];
        fast_float(s)
    }

    fn lex_string(&mut self) {
        self.pos += 1; // consume '('
        let start = self.scratch.len();
        let mut depth = 1usize;
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            self.pos += 1;
            match b {
                b'\\' => {
                    if self.pos >= self.data.len() {
                        break;
                    }
                    let e = self.data[self.pos];
                    self.pos += 1;
                    match e {
                        b'n' => self.scratch.push(b'\n'),
                        b'r' => self.scratch.push(b'\r'),
                        b't' => self.scratch.push(b'\t'),
                        b'b' => self.scratch.push(0x08),
                        b'f' => self.scratch.push(0x0c),
                        b'(' | b')' | b'\\' => self.scratch.push(e),
                        b'\r' => {
                            // line continuation; swallow optional \n
                            if self.data.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.data.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            self.scratch.push(v as u8);
                        }
                        other => self.scratch.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    self.scratch.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    self.scratch.push(b);
                }
                _ => self.scratch.push(b),
            }
        }
        self.operands.push(Operand::Str {
            start,
            len: self.scratch.len() - start,
        });
    }

    fn lex_hex_string(&mut self) {
        self.pos += 1; // consume '<'
        let start = self.scratch.len();
        let mut hi: Option<u8> = None;
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            self.pos += 1;
            if b == b'>' {
                break;
            }
            let v = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => continue,
            };
            match hi.take() {
                Some(h) => self.scratch.push((h << 4) | v),
                None => hi = Some(v),
            }
        }
        if let Some(h) = hi {
            self.scratch.push(h << 4); // odd digit: low nibble zero
        }
        self.operands.push(Operand::Str {
            start,
            len: self.scratch.len() - start,
        });
    }

    fn skip_dict(&mut self) {
        // << … >> possibly nested; strings inside may contain >>. The
        // raw span is preserved as an operand (BDC properties carry
        // /ActualText).
        let dict_start = self.pos;
        self.pos += 2;
        let mut depth = 1usize;
        while self.pos < self.data.len() && depth > 0 {
            match self.data[self.pos] {
                b'<' if self.data.get(self.pos + 1) == Some(&b'<') => {
                    depth += 1;
                    self.pos += 2;
                }
                b'>' if self.data.get(self.pos + 1) == Some(&b'>') => {
                    depth -= 1;
                    self.pos += 2;
                }
                b'(' => {
                    let keep = self.scratch.len();
                    self.lex_string();
                    self.operands.pop();
                    self.scratch.truncate(keep);
                }
                _ => self.pos += 1,
            }
        }
        self.operands.push(Operand::Dict {
            start: dict_start,
            len: self.pos - dict_start,
        });
    }

    fn skip_inline_image(&mut self) {
        // BI <dict entries> ID <binary…> EI
        // Find "ID", skip one whitespace, then scan for whitespace-delimited EI.
        while self.pos + 1 < self.data.len() {
            match self.data[self.pos] {
                b'(' => {
                    self.pos += 1;
                    let mut depth = 1usize;
                    while self.pos < self.data.len() && depth > 0 {
                        match self.data[self.pos] {
                            b'\\' => self.pos = (self.pos + 2).min(self.data.len()),
                            b'(' => {
                                depth += 1;
                                self.pos += 1;
                            }
                            b')' => {
                                depth -= 1;
                                self.pos += 1;
                            }
                            _ => self.pos += 1,
                        }
                    }
                }
                b'%' => {
                    while self.pos < self.data.len()
                        && !matches!(self.data[self.pos], b'\r' | b'\n')
                    {
                        self.pos += 1;
                    }
                }
                b'I' if self.data[self.pos + 1] == b'D'
                    && (self.pos == 0 || is_ws(self.data[self.pos - 1]))
                    && self.data.get(self.pos + 2).is_some_and(|byte| is_ws(*byte)) =>
                {
                    self.pos += 3;
                    break;
                }
                _ => self.pos += 1,
            }
        }
        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == b'E'
                && self.data[self.pos + 1] == b'I'
                && (self.pos == 0 || is_ws(self.data[self.pos - 1]))
                && self
                    .data
                    .get(self.pos + 2)
                    .is_none_or(|b| is_ws(*b) || is_delim(*b))
                && plausible_post_inline_syntax(&self.data[self.pos + 2..])
            {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
        self.pos = self.data.len();
    }
}

/// Fast decimal float parse for content-stream numbers (no exponents).
pub fn fast_float_pub(s: &[u8]) -> f64 {
    fast_float(s)
}

fn fast_float(s: &[u8]) -> f64 {
    let mut i = 0usize;
    let neg = match s.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let mut int_part = 0f64;
    while i < s.len() && s[i].is_ascii_digit() {
        int_part = int_part * 10.0 + (s[i] - b'0') as f64;
        i += 1;
    }
    let mut frac = 0f64;
    let mut scale = 1f64;
    if i < s.len() && s[i] == b'.' {
        i += 1;
        while i < s.len() && s[i].is_ascii_digit() {
            frac = frac * 10.0 + (s[i] - b'0') as f64;
            scale *= 10.0;
            i += 1;
        }
    }
    let v = int_part + frac / scale;
    if neg {
        -v
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(data: &[u8]) -> Vec<(String, usize)> {
        let mut lex = Lexer::new(data);
        let mut out = Vec::new();
        while let Some(op) = lex.next_op() {
            out.push((String::from_utf8_lossy(op).into_owned(), lex.operands.len()));
            lex.clear();
        }
        out
    }

    #[test]
    fn basic_text_ops() {
        let r = ops(b"BT /F1 12 Tf 10 20 Td (Hi) Tj ET");
        assert_eq!(
            r,
            vec![
                ("BT".into(), 0),
                ("Tf".into(), 2),
                ("Td".into(), 2),
                ("Tj".into(), 1),
                ("ET".into(), 0)
            ]
        );
    }

    #[test]
    fn tj_array_and_escapes() {
        let mut lex = Lexer::new(b"[(a\\(b\\)) -120 (c\\151d)] TJ");
        let op = lex.next_op().unwrap();
        assert_eq!(op, b"TJ");
        // ArrStart, Str, Num, Str
        assert_eq!(lex.operands.len(), 4);
        assert_eq!(lex.str_bytes(lex.operands[1]), b"a(b)");
        assert_eq!(lex.str_bytes(lex.operands[3]), b"cid"); // \151 = 'i'
    }

    #[test]
    fn hex_string_and_dict_skip() {
        let r = ops(b"<48690a> Tj /Span <</MCID 3>> BDC");
        assert_eq!(r[0].0, "Tj");
        assert_eq!(r[1].0, "BDC");
    }

    #[test]
    fn inline_image_skipped() {
        // The payload is skipped; BI itself surfaces as an operator so
        // interpreters can record the placement.
        let r = ops(b"q BI /W 4 /H 4 ID \x00\x01\x02 EI Q (x) Tj");
        assert_eq!(
            r.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(),
            vec!["q", "BI", "Q", "Tj"]
        );
    }

    #[test]
    fn numbers() {
        let mut lex = Lexer::new(b"1.02 -779 +3 .5 Tz");
        lex.next_op().unwrap();
        let v: Vec<f64> = lex
            .operands
            .iter()
            .map(|o| match o {
                Operand::Num(n) => *n,
                _ => f64::NAN,
            })
            .collect();
        assert_eq!(v, vec![1.02, -779.0, 3.0, 0.5]);
    }

    #[test]
    fn inline_image_binary_ei_sequence_does_not_desynchronize_operators() {
        let mut lex = Lexer::new(b"BI /W 1 /H 1 ID abc EI\0spoof EI 1 0 0 1 2 3 cm Q");
        assert_eq!(lex.next_op(), Some(b"BI".as_slice()));
        lex.clear();
        assert_eq!(lex.next_op(), Some(b"cm".as_slice()));
        lex.clear();
        assert_eq!(lex.next_op(), Some(b"Q".as_slice()));
        lex.clear();
        assert_eq!(lex.next_op(), None);
    }

    #[test]
    fn post_inline_operator_probe_accepts_normal_suffix() {
        assert!(plausible_post_inline_syntax(b" Q (x) Tj"));
    }

    #[test]
    fn inline_image_rejects_ei_followed_by_binary_garbage() {
        let mut lex = Lexer::new(b"BI /W 1 /H 1 ID abc EI \xFF\xD8\xFF EI Q");
        assert_eq!(lex.next_op(), Some(b"BI".as_slice()));
        lex.clear();
        assert_eq!(lex.next_op(), Some(b"Q".as_slice()));
    }

    #[test]
    fn inline_image_id_inside_dictionary_string_is_not_data_start() {
        let mut lex = Lexer::new(b"BI /Metadata (fake ID bytes EI Q) /W 1 /H 1 ID abc EI Q");
        assert_eq!(lex.next_op(), Some(b"BI".as_slice()));
        lex.clear();
        assert_eq!(lex.next_op(), Some(b"Q".as_slice()));
        lex.clear();
        assert_eq!(lex.next_op(), None);
    }

    #[test]
    fn inline_image_ei_at_end_and_truncated_data_terminate_cleanly() {
        for content in [
            b"BI /W 1 /H 1 ID abc EI".as_slice(),
            b"BI /W 1 /H 1 ID abc".as_slice(),
            b"BI /W 1 /H 1".as_slice(),
        ] {
            let mut lex = Lexer::new(content);
            assert_eq!(lex.next_op(), Some(b"BI".as_slice()));
            lex.clear();
            assert_eq!(lex.next_op(), None);
        }
    }
}
