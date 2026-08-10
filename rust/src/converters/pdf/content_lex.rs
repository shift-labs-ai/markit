//! Zero-copy streaming lexer for PDF content streams.
//!
//! Replaces lopdf's Content::decode on the fast path: no operation list,
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

#[inline]
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\0')
}

#[inline]
fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
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
                        self.skip_dict();
                        self.operands.push(Operand::Other);
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
        // << … >> possibly nested; strings inside may contain >>.
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
    }

    fn skip_inline_image(&mut self) {
        // BI <dict entries> ID <binary…> EI
        // Find "ID", skip one whitespace, then scan for whitespace-delimited EI.
        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == b'I' && self.data[self.pos + 1] == b'D' {
                self.pos += 2;
                if self.pos < self.data.len() && is_ws(self.data[self.pos]) {
                    self.pos += 1;
                }
                break;
            }
            self.pos += 1;
        }
        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == b'E'
                && self.data[self.pos + 1] == b'I'
                && (self.pos == 0 || is_ws(self.data[self.pos - 1]))
                && self
                    .data
                    .get(self.pos + 2)
                    .is_none_or(|b| is_ws(*b) || is_delim(*b))
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
        let r = ops(b"q BI /W 4 /H 4 ID \x00\x01\x02 EI Q (x) Tj");
        assert_eq!(
            r.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(),
            vec!["q", "Q", "Tj"]
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
}
