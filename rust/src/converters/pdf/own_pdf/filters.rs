//! PDF stream filters and predictor reversal. Decryption is applied
//! before the declared filter chain. Supported filters: Flate, LZW,
//! ASCIIHex, ASCII85, RunLength, with PNG predictors.

use anyhow::{anyhow, bail, Result};

use super::document::Pdf;
use super::values::{Dict, Val};

pub fn decode_stream<'a>(dict: &Dict<'a>, raw: &[u8], pdf: &Pdf<'a>) -> Result<Vec<u8>> {
    // Decryption applies to the raw bytes, before any filter. Streams
    // reached before setup (the xref stream itself) are never encrypted.
    let decrypted;
    let raw: &[u8] = if pdf.decrypt.is_some() {
        decrypted = pdf.decrypt_stream(raw)?;
        &decrypted
    } else {
        raw
    };

    let filter = pdf.dict_get(dict, b"Filter")?;
    let mut out = match filter {
        None => raw.to_vec(),
        Some(Val::Name(n)) => apply_filter(n, raw)?,
        Some(Val::Array(fs)) => {
            let mut cur = raw.to_vec();
            for f in fs {
                let Val::Name(n) = pdf.resolve(&f)? else {
                    bail!("bad filter entry");
                };
                cur = apply_filter(n, &cur)?;
            }
            cur
        }
        _ => bail!("bad Filter"),
    };

    // Predictors (xref streams, ObjStm): PNG predictors only.
    let parms = pdf.dict_get(dict, b"DecodeParms")?;
    if let Some(Val::Dict(p)) = parms {
        let predictor = pdf
            .dict_get(&p, b"Predictor")?
            .and_then(|v| v.as_num())
            .unwrap_or(1.0) as i64;
        if predictor >= 10 {
            let columns = pdf
                .dict_get(&p, b"Columns")?
                .and_then(|v| v.as_num())
                .unwrap_or(1.0) as usize;
            let colors = pdf
                .dict_get(&p, b"Colors")?
                .and_then(|v| v.as_num())
                .unwrap_or(1.0) as usize;
            out = png_unpredict(&out, columns * colors)?;
        } else if predictor != 1 {
            bail!("unsupported predictor");
        }
    }
    Ok(out)
}

fn apply_filter(name: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    match name {
        b"FlateDecode" | b"Fl" => inflate(data),
        b"ASCIIHexDecode" | b"AHx" => {
            let mut out = Vec::with_capacity(data.len() / 2);
            let mut hi: Option<u8> = None;
            for &b in data {
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
                    Some(h) => out.push((h << 4) | v),
                    None => hi = Some(v),
                }
            }
            if let Some(h) = hi {
                out.push(h << 4);
            }
            Ok(out)
        }
        b"ASCII85Decode" | b"A85" => ascii85(data),
        b"LZWDecode" | b"LZW" => lzw_decode(data),
        b"RunLengthDecode" | b"RL" => {
            let mut out = Vec::with_capacity(data.len() * 2);
            let mut i = 0usize;
            while i < data.len() {
                let l = data[i];
                i += 1;
                match l {
                    0..=127 => {
                        let n = l as usize + 1;
                        if i + n > data.len() {
                            break;
                        }
                        out.extend_from_slice(&data[i..i + n]);
                        i += n;
                    }
                    128 => break, // EOD
                    _ => {
                        if i >= data.len() {
                            break;
                        }
                        out.extend(std::iter::repeat_n(data[i], 257 - l as usize));
                        i += 1;
                    }
                }
            }
            Ok(out)
        }
        _ => bail!("unsupported filter {}", String::from_utf8_lossy(name)),
    }
}

/// LZW as PDF uses it (MSB-first codes, 9–12 bits, EarlyChange=1).
fn lzw_decode(data: &[u8]) -> Result<Vec<u8>> {
    const CLEAR: u16 = 256;
    const EOD: u16 = 257;

    let mut out = Vec::with_capacity(data.len() * 3);
    let mut dict: Vec<Vec<u8>> = (0..=257u16).map(|i| vec![i as u8]).collect();
    let mut code_len = 9usize;
    let mut prev: Option<u16> = None;

    let mut bitbuf = 0u32;
    let mut bits = 0usize;
    let mut i = 0usize;

    loop {
        while bits < code_len {
            if i >= data.len() {
                return Ok(out);
            }
            bitbuf = (bitbuf << 8) | data[i] as u32;
            bits += 8;
            i += 1;
        }
        let code = ((bitbuf >> (bits - code_len)) & ((1 << code_len) - 1)) as u16;
        bits -= code_len;

        match code {
            CLEAR => {
                dict.truncate(258);
                code_len = 9;
                prev = None;
            }
            EOD => return Ok(out),
            _ => {
                let entry: Vec<u8> = if (code as usize) < dict.len() {
                    dict[code as usize].clone()
                } else if let Some(p) = prev {
                    // KwKwK case
                    let mut e = dict[p as usize].clone();
                    e.push(dict[p as usize][0]);
                    e
                } else {
                    bail!("bad LZW stream");
                };
                out.extend_from_slice(&entry);
                if let Some(p) = prev {
                    let mut ne = dict[p as usize].clone();
                    ne.push(entry[0]);
                    dict.push(ne);
                }
                prev = Some(code);
                // EarlyChange=1: widen one code early.
                if dict.len() + 1 >= (1 << code_len) && code_len < 12 {
                    code_len += 1;
                }
            }
        }
    }
}

fn ascii85(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 4 / 5);
    let mut group = [0u8; 5];
    let mut n = 0usize;
    let mut i = 0usize;
    // optional <~ prefix
    if data.starts_with(b"<~") {
        i = 2;
    }
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            b'~' => break, // ~> EOD
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group[n] = b - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u32, |a, &d| a * 85 + d as u32);
                    out.extend_from_slice(&v.to_be_bytes());
                    n = 0;
                }
            }
            _ => {} // whitespace
        }
    }
    if n > 0 {
        // Partial group: pad with 'u' (84), emit n-1 bytes.
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u32, |a, &d| a * 85 + d as u32);
        out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
    }
    Ok(out)
}

/// Public shim for image extraction.
pub fn inflate_pub(data: &[u8]) -> Result<Vec<u8>> {
    inflate(data)
}

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::with_capacity(data.len() * 4);
    flate2::read::ZlibDecoder::new(data)
        .read_to_end(&mut out)
        .map_err(|e| anyhow!("inflate: {e}"))?;
    Ok(out)
}

fn png_unpredict(data: &[u8], row_len: usize) -> Result<Vec<u8>> {
    if row_len == 0 {
        bail!("bad predictor columns");
    }
    let stride = row_len
        .checked_add(1)
        .ok_or_else(|| anyhow!("predictor row overflow"))?;
    if !data.len().is_multiple_of(stride) {
        bail!("truncated PNG predictor row");
    }
    let rows = data.len() / stride;
    let out_len = rows
        .checked_mul(row_len)
        .ok_or_else(|| anyhow!("predictor output overflow"))?;
    let mut out = vec![0u8; out_len];
    for r in 0..rows {
        let ftype = data[r * stride];
        let src = &data[r * stride + 1..r * stride + stride];
        for c in 0..row_len {
            let left = if c > 0 { out[r * row_len + c - 1] } else { 0 };
            let up = if r > 0 { out[(r - 1) * row_len + c] } else { 0 };
            let ul = if r > 0 && c > 0 {
                out[(r - 1) * row_len + c - 1]
            } else {
                0
            };
            let v = match ftype {
                0 => src[c],
                1 => src[c].wrapping_add(left),
                2 => src[c].wrapping_add(up),
                3 => src[c].wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => {
                    // Paeth
                    let p = left as i16 + up as i16 - ul as i16;
                    let (pa, pb, pc) = (
                        (p - left as i16).abs(),
                        (p - up as i16).abs(),
                        (p - ul as i16).abs(),
                    );
                    let pred = if pa <= pb && pa <= pc {
                        left
                    } else if pb <= pc {
                        up
                    } else {
                        ul
                    };
                    src[c].wrapping_add(pred)
                }
                _ => bail!("bad PNG predictor row"),
            };
            out[r * row_len + c] = v;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pack_9bit(codes: &[u16]) -> Vec<u8> {
        let mut bits = Vec::new();
        for &c in codes {
            for shift in (0..9).rev() {
                bits.push(((c >> shift) & 1) as u8);
            }
        }
        let mut out = Vec::new();
        for chunk in bits.chunks(8) {
            let mut b = 0u8;
            for (i, &bit) in chunk.iter().enumerate() {
                b |= bit << (7 - i);
            }
            out.push(b);
        }
        out
    }

    #[test]
    fn every_filter_decodes_known_payload() {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(b"flate").unwrap();
        let z = z.finish().unwrap();
        assert_eq!(apply_filter(b"FlateDecode", &z).unwrap(), b"flate");
        assert_eq!(apply_filter(b"ASCIIHexDecode", b"486578>").unwrap(), b"Hex");
        assert_eq!(
            apply_filter(b"ASCII85Decode", b"<~87cURD_*#TDfTZ)+T~>").unwrap(),
            b"Hello, world!"
        );
        let lzw = pack_9bit(&[256, b'L' as u16, b'Z' as u16, b'W' as u16, 257]);
        assert_eq!(apply_filter(b"LZWDecode", &lzw).unwrap(), b"LZW");
        assert_eq!(
            apply_filter(b"RunLengthDecode", &[2, b'R', b'L', b'E', 128]).unwrap(),
            b"RLE"
        );
    }

    #[test]
    fn filter_chain_order_is_observable() {
        // ASCIIHex decodes to an ASCII85 payload, which then decodes to
        // "Hello, world!". Reversing the chain cannot produce it.
        let hex = b"3c7e3837635552445f2a23544466545a292b547e3e>";
        let a85 = apply_filter(b"ASCIIHexDecode", hex).unwrap();
        let plain = apply_filter(b"ASCII85Decode", &a85).unwrap();
        assert_eq!(plain, b"Hello, world!");
        assert_ne!(
            apply_filter(b"ASCII85Decode", hex).unwrap(),
            b"Hello, world!"
        );
    }

    #[test]
    fn truncated_predictor_row_is_rejected() {
        assert!(png_unpredict(&[0, 1, 2], 3).is_err());
        assert!(png_unpredict(&[], usize::MAX).is_err());
    }

    #[test]
    fn truncated_filters_return_without_panicking() {
        for name in [
            b"FlateDecode".as_slice(),
            b"ASCIIHexDecode",
            b"ASCII85Decode",
            b"LZWDecode",
            b"RunLengthDecode",
        ] {
            for data in [&b""[..], &b"x"[..], &b"~"[..], &[0xff, 0xff, 0xff][..]] {
                let r = std::panic::catch_unwind(|| apply_filter(name, data));
                assert!(r.is_ok(), "{} panicked", String::from_utf8_lossy(name));
            }
        }
    }
}
