//! CCITT Group 3/4 fax decoding (CCITTFaxDecode filter).
//!
//! Supports K<0 (pure 2D, G4), K=0 (1D MH), K>0 (mixed 1D/2D G3),
//! EncodedByteAlign, BlackIs1, Columns/Rows. Output is one byte per
//! pixel (0 black, 255 white), ready for PNG grayscale packing.

use anyhow::{bail, Result};

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // bit position
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    fn peek(&self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            let p = self.pos + i as usize;
            let bit = if p / 8 < self.data.len() {
                (self.data[p / 8] >> (7 - p % 8)) & 1
            } else {
                0
            };
            v = (v << 1) | bit as u32;
        }
        v
    }

    fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    fn byte_align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    fn eod(&self) -> bool {
        self.pos >= self.data.len() * 8
    }
}

/// (bits, len, run) code tables — ITU T.4 terminating + makeup codes.
const WHITE_CODES: &[(u16, u8, u16)] = &[
    (0x35, 8, 0),
    (0x07, 6, 1),
    (0x07, 4, 2),
    (0x08, 4, 3),
    (0x0B, 4, 4),
    (0x0C, 4, 5),
    (0x0E, 4, 6),
    (0x0F, 4, 7),
    (0x13, 5, 8),
    (0x14, 5, 9),
    (0x07, 5, 10),
    (0x08, 5, 11),
    (0x08, 6, 12),
    (0x03, 6, 13),
    (0x34, 6, 14),
    (0x35, 6, 15),
    (0x2A, 6, 16),
    (0x2B, 6, 17),
    (0x27, 7, 18),
    (0x0C, 7, 19),
    (0x08, 7, 20),
    (0x17, 7, 21),
    (0x03, 7, 22),
    (0x04, 7, 23),
    (0x28, 7, 24),
    (0x2B, 7, 25),
    (0x13, 7, 26),
    (0x24, 7, 27),
    (0x18, 7, 28),
    (0x02, 8, 29),
    (0x03, 8, 30),
    (0x1A, 8, 31),
    (0x1B, 8, 32),
    (0x12, 8, 33),
    (0x13, 8, 34),
    (0x14, 8, 35),
    (0x15, 8, 36),
    (0x16, 8, 37),
    (0x17, 8, 38),
    (0x28, 8, 39),
    (0x29, 8, 40),
    (0x2A, 8, 41),
    (0x2B, 8, 42),
    (0x2C, 8, 43),
    (0x2D, 8, 44),
    (0x04, 8, 45),
    (0x05, 8, 46),
    (0x0A, 8, 47),
    (0x0B, 8, 48),
    (0x52, 8, 49),
    (0x53, 8, 50),
    (0x54, 8, 51),
    (0x55, 8, 52),
    (0x24, 8, 53),
    (0x25, 8, 54),
    (0x58, 8, 55),
    (0x59, 8, 56),
    (0x5A, 8, 57),
    (0x5B, 8, 58),
    (0x4A, 8, 59),
    (0x4B, 8, 60),
    (0x32, 8, 61),
    (0x33, 8, 62),
    (0x34, 8, 63),
    // makeup
    (0x1B, 5, 64),
    (0x12, 5, 128),
    (0x17, 6, 192),
    (0x37, 7, 256),
    (0x36, 8, 320),
    (0x37, 8, 384),
    (0x64, 8, 448),
    (0x65, 8, 512),
    (0x68, 8, 576),
    (0x67, 8, 640),
    (0xCC, 9, 704),
    (0xCD, 9, 768),
    (0xD2, 9, 832),
    (0xD3, 9, 896),
    (0xD4, 9, 960),
    (0xD5, 9, 1024),
    (0xD6, 9, 1088),
    (0xD7, 9, 1152),
    (0xD8, 9, 1216),
    (0xD9, 9, 1280),
    (0xDA, 9, 1344),
    (0xDB, 9, 1408),
    (0x98, 9, 1472),
    (0x99, 9, 1536),
    (0x9A, 9, 1600),
    (0x18, 6, 1664),
    (0x9B, 9, 1728),
    // extended makeup (common)
    (0x08, 11, 1792),
    (0x0C, 11, 1856),
    (0x0D, 11, 1920),
    (0x12, 12, 1984),
    (0x13, 12, 2048),
    (0x14, 12, 2112),
    (0x15, 12, 2176),
    (0x16, 12, 2240),
    (0x17, 12, 2304),
    (0x1C, 12, 2368),
    (0x1D, 12, 2432),
    (0x1E, 12, 2496),
    (0x1F, 12, 2560),
];

const BLACK_CODES: &[(u16, u8, u16)] = &[
    (0x37, 10, 0),
    (0x02, 3, 1),
    (0x03, 2, 2),
    (0x02, 2, 3),
    (0x03, 3, 4),
    (0x03, 4, 5),
    (0x02, 4, 6),
    (0x03, 5, 7),
    (0x05, 6, 8),
    (0x04, 6, 9),
    (0x04, 7, 10),
    (0x05, 7, 11),
    (0x07, 7, 12),
    (0x04, 8, 13),
    (0x07, 8, 14),
    (0x18, 9, 15),
    (0x17, 10, 16),
    (0x18, 10, 17),
    (0x08, 10, 18),
    (0x67, 11, 19),
    (0x68, 11, 20),
    (0x6C, 11, 21),
    (0x37, 11, 22),
    (0x28, 11, 23),
    (0x17, 11, 24),
    (0x18, 11, 25),
    (0xCA, 12, 26),
    (0xCB, 12, 27),
    (0xCC, 12, 28),
    (0xCD, 12, 29),
    (0x68, 12, 30),
    (0x69, 12, 31),
    (0x6A, 12, 32),
    (0x6B, 12, 33),
    (0xD2, 12, 34),
    (0xD3, 12, 35),
    (0xD4, 12, 36),
    (0xD5, 12, 37),
    (0xD6, 12, 38),
    (0xD7, 12, 39),
    (0x6C, 12, 40),
    (0x6D, 12, 41),
    (0xDA, 12, 42),
    (0xDB, 12, 43),
    (0x54, 12, 44),
    (0x55, 12, 45),
    (0x56, 12, 46),
    (0x57, 12, 47),
    (0x64, 12, 48),
    (0x65, 12, 49),
    (0x52, 12, 50),
    (0x53, 12, 51),
    (0x24, 12, 52),
    (0x37, 12, 53),
    (0x38, 12, 54),
    (0x27, 12, 55),
    (0x28, 12, 56),
    (0x58, 12, 57),
    (0x59, 12, 58),
    (0x2B, 12, 59),
    (0x2C, 12, 60),
    (0x5A, 12, 61),
    (0x66, 12, 62),
    (0x67, 12, 63),
    // makeup
    (0x0F, 10, 64),
    (0xC8, 12, 128),
    (0xC9, 12, 192),
    (0x5B, 12, 256),
    (0x33, 12, 320),
    (0x34, 12, 384),
    (0x35, 12, 448),
    (0x6C, 13, 512),
    (0x6D, 13, 576),
    (0x4A, 13, 640),
    (0x4B, 13, 704),
    (0x4C, 13, 768),
    (0x4D, 13, 832),
    (0x72, 13, 896),
    (0x73, 13, 960),
    (0x74, 13, 1024),
    (0x75, 13, 1088),
    (0x76, 13, 1152),
    (0x77, 13, 1216),
    (0x52, 13, 1280),
    (0x53, 13, 1344),
    (0x54, 13, 1408),
    (0x55, 13, 1472),
    (0x5A, 13, 1536),
    (0x5B, 13, 1600),
    (0x64, 13, 1664),
    (0x65, 13, 1728),
    (0x08, 11, 1792),
    (0x0C, 11, 1856),
    (0x0D, 11, 1920),
    (0x12, 12, 1984),
    (0x13, 12, 2048),
    (0x14, 12, 2112),
    (0x15, 12, 2176),
    (0x16, 12, 2240),
    (0x17, 12, 2304),
    (0x1C, 12, 2368),
    (0x1D, 12, 2432),
    (0x1E, 12, 2496),
    (0x1F, 12, 2560),
];

fn read_run(br: &mut BitReader, white: bool) -> Result<u32> {
    let table = if white { WHITE_CODES } else { BLACK_CODES };
    let mut total = 0u32;
    loop {
        let mut hit = None;
        for &(bits, len, run) in table {
            if br.peek(len as u32) == bits as u32 {
                hit = Some((len, run));
                break;
            }
        }
        let Some((len, run)) = hit else {
            bail!("bad {} run code", if white { "white" } else { "black" });
        };
        br.skip(len as u32);
        total += run as u32;
        if run < 64 {
            return Ok(total);
        }
        // makeup code: a terminating code follows
    }
}

/// Decode one 1D (MH) line into changing positions.
fn decode_1d(br: &mut BitReader, columns: usize) -> Result<Vec<usize>> {
    let mut changes = Vec::new();
    let mut pos = 0usize;
    let mut white = true;
    while pos < columns {
        let run = read_run(br, white)? as usize;
        pos = (pos + run).min(columns);
        changes.push(pos);
        white = !white;
    }
    Ok(changes)
}

/// Decode one 2D line against the reference line's changing elements.
fn decode_2d(br: &mut BitReader, reference: &[usize], columns: usize) -> Result<Vec<usize>> {
    let mut changes: Vec<usize> = Vec::new();
    let mut a0: isize = -1;
    let mut white = true;

    // b1: first changing element on reference line to the right of a0
    // with opposite colour of a0's colour run.
    let find_b1 = |a0: isize, white: bool, changes_len: usize| -> usize {
        let _ = changes_len;
        // reference changes alternate starting with white->black
        let mut i = 0usize;
        while i < reference.len() {
            let c = reference[i] as isize;
            // parity: even index = white->black transition
            let is_w2b = i.is_multiple_of(2);
            if c > a0 && (is_w2b == white) {
                return reference[i];
            }
            i += 1;
        }
        columns
    };

    while (a0 as usize) < columns || a0 < 0 {
        let b1 = find_b1(a0, white, changes.len());
        let b2 = {
            let mut val = columns;
            for &c in reference {
                if c > b1 {
                    val = c;
                    break;
                }
            }
            val
        };

        if br.eod() {
            break;
        }

        // Mode codes.
        if br.peek(1) == 1 {
            // V0
            br.skip(1);
            changes.push(b1);
            a0 = b1 as isize;
            white = !white;
        } else if br.peek(3) == 0b001 {
            // Horizontal
            br.skip(3);
            let r1 = read_run(br, white)? as usize;
            let r2 = read_run(br, !white)? as usize;
            let start = if a0 < 0 { 0 } else { a0 as usize };
            let p1 = (start + r1).min(columns);
            let p2 = (p1 + r2).min(columns);
            changes.push(p1);
            changes.push(p2);
            a0 = p2 as isize;
        } else if br.peek(4) == 0b0001 {
            // Pass
            br.skip(4);
            a0 = b2 as isize;
        } else if br.peek(3) == 0b011 {
            br.skip(3); // VR1
            let p = (b1 + 1).min(columns);
            changes.push(p);
            a0 = p as isize;
            white = !white;
        } else if br.peek(3) == 0b010 {
            br.skip(3); // VL1
            let p = b1.saturating_sub(1);
            changes.push(p);
            a0 = p as isize;
            white = !white;
        } else if br.peek(6) == 0b000011 {
            br.skip(6); // VR2
            let p = (b1 + 2).min(columns);
            changes.push(p);
            a0 = p as isize;
            white = !white;
        } else if br.peek(6) == 0b000010 {
            br.skip(6); // VL2
            let p = b1.saturating_sub(2);
            changes.push(p);
            a0 = p as isize;
            white = !white;
        } else if br.peek(7) == 0b0000011 {
            br.skip(7); // VR3
            let p = (b1 + 3).min(columns);
            changes.push(p);
            a0 = p as isize;
            white = !white;
        } else if br.peek(7) == 0b0000010 {
            br.skip(7); // VL3
            let p = b1.saturating_sub(3);
            changes.push(p);
            a0 = p as isize;
            white = !white;
        } else if br.peek(12) == 1 {
            // EOL
            br.skip(12);
            break;
        } else {
            bail!("bad 2D mode code");
        }
        if a0 as usize >= columns {
            break;
        }
    }
    changes.retain(|&c| c <= columns);
    Ok(changes)
}

fn render_line(changes: &[usize], columns: usize, out: &mut Vec<u8>) {
    let start = out.len();
    out.resize(start + columns, 255);
    let mut white = true;
    let mut pos = 0usize;
    for &c in changes {
        if !white {
            for p in out.iter_mut().take(start + c).skip(start + pos) {
                *p = 0;
            }
        }
        pos = c;
        white = !white;
    }
    if !white {
        for p in out.iter_mut().take(start + columns).skip(start + pos) {
            *p = 0;
        }
    }
}

/// Decode CCITT data to 8-bit grayscale rows (columns × rows bytes).
pub fn decode(
    data: &[u8],
    k: i32,
    columns: usize,
    rows: usize,
    byte_align: bool,
    black_is_1: bool,
) -> Result<Vec<u8>> {
    if columns == 0 || columns > 30_000 {
        bail!("bad Columns");
    }
    let mut br = BitReader::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(columns * rows.max(1));
    // Imaginary all-white reference line.
    let mut reference: Vec<usize> = vec![columns, columns];
    let mut line_no = 0usize;

    while !br.eod() && (rows == 0 || line_no < rows) {
        if byte_align {
            br.byte_align();
        }
        // Skip EOL(s) 000000000001 (+ mode bit for G3 2D).
        let mut two_d = k < 0;
        while br.peek(12) == 1 {
            br.skip(12);
            if k > 0 {
                two_d = br.peek(1) == 0;
                br.skip(1);
            }
        }
        if br.eod() {
            break;
        }
        let changes = if two_d {
            decode_2d(&mut br, &reference, columns)?
        } else {
            decode_1d(&mut br, columns)?
        };
        if changes.is_empty() && br.eod() {
            break;
        }
        render_line(&changes, columns, &mut out);
        reference = changes;
        if reference.last() != Some(&columns) {
            reference.push(columns);
            reference.push(columns);
        }
        line_no += 1;
    }

    if black_is_1 {
        for p in out.iter_mut() {
            *p = 255 - *p;
        }
    }
    if rows > 0 {
        out.resize(columns * rows, 255);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two all-white 8px lines in G4: V0 (bit 1) once per line — the
    /// single transition lands at b1 = columns.
    #[test]
    fn g4_all_white() {
        let out = decode(&[0b1100_0000], -1, 8, 2, false, false).unwrap();
        assert_eq!(out.len(), 16);
        assert!(out.iter().all(|&p| p == 255));
    }

    /// One line via horizontal mode: 001, white run 0 (0x35/8), black
    /// run 8 (0x05/6) — a fully black 8px line.
    #[test]
    fn g4_horizontal_black_line() {
        // bits: 001 00110101 000101 → 0b0010_0110, 0b1010_0010, 0b1000_0000
        let data = [0b0010_0110, 0b1010_0010, 0b1000_0000];
        let out = decode(&data, -1, 8, 1, false, false).unwrap();
        assert_eq!(out, vec![0u8; 8]);
    }

    /// BlackIs1 inverts the rendered polarity.
    #[test]
    fn black_is_1_inverts() {
        let out = decode(&[0b1100_0000], -1, 8, 2, false, true).unwrap();
        assert!(out.iter().all(|&p| p == 0));
    }
}
