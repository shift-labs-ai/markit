//! Predefined CJK CMaps (Adobe cmap-resources, BSD-3-licensed data).
//!
//! Each embedded blob is a deflate-raw compressed table compiled from
//! Adobe's CMap text files: codespace ranges (how many bytes a code
//! takes) plus sorted cid ranges. A per-ordering CID->Unicode table
//! (compiled from mapping-resources-pdf) turns CIDs into text.

use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ordering {
    GB1,
    CNS1,
    Japan1,
    Korea1,
}

static ENCODINGS: &[(&[u8], &[u8], Ordering)] = &[
    (
        b"GB-EUC-H",
        include_bytes!("cmaps/GB-EUC-H.bin.z"),
        Ordering::GB1,
    ),
    (
        b"GB-EUC-V",
        include_bytes!("cmaps/GB-EUC-V.bin.z"),
        Ordering::GB1,
    ),
    (
        b"GBpc-EUC-H",
        include_bytes!("cmaps/GBpc-EUC-H.bin.z"),
        Ordering::GB1,
    ),
    (
        b"GBpc-EUC-V",
        include_bytes!("cmaps/GBpc-EUC-V.bin.z"),
        Ordering::GB1,
    ),
    (
        b"GBK-EUC-H",
        include_bytes!("cmaps/GBK-EUC-H.bin.z"),
        Ordering::GB1,
    ),
    (
        b"GBK-EUC-V",
        include_bytes!("cmaps/GBK-EUC-V.bin.z"),
        Ordering::GB1,
    ),
    (
        b"GBKp-EUC-H",
        include_bytes!("cmaps/GBKp-EUC-H.bin.z"),
        Ordering::GB1,
    ),
    (
        b"GBKp-EUC-V",
        include_bytes!("cmaps/GBKp-EUC-V.bin.z"),
        Ordering::GB1,
    ),
    (
        b"GBK2K-H",
        include_bytes!("cmaps/GBK2K-H.bin.z"),
        Ordering::GB1,
    ),
    (
        b"GBK2K-V",
        include_bytes!("cmaps/GBK2K-V.bin.z"),
        Ordering::GB1,
    ),
    (
        b"B5pc-H",
        include_bytes!("cmaps/B5pc-H.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"B5pc-V",
        include_bytes!("cmaps/B5pc-V.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"ETen-B5-H",
        include_bytes!("cmaps/ETen-B5-H.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"ETen-B5-V",
        include_bytes!("cmaps/ETen-B5-V.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"ETenms-B5-H",
        include_bytes!("cmaps/ETenms-B5-H.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"ETenms-B5-V",
        include_bytes!("cmaps/ETenms-B5-V.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"CNS-EUC-H",
        include_bytes!("cmaps/CNS-EUC-H.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"CNS-EUC-V",
        include_bytes!("cmaps/CNS-EUC-V.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"HKscs-B5-H",
        include_bytes!("cmaps/HKscs-B5-H.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"HKscs-B5-V",
        include_bytes!("cmaps/HKscs-B5-V.bin.z"),
        Ordering::CNS1,
    ),
    (
        b"83pv-RKSJ-H",
        include_bytes!("cmaps/83pv-RKSJ-H.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"90ms-RKSJ-H",
        include_bytes!("cmaps/90ms-RKSJ-H.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"90ms-RKSJ-V",
        include_bytes!("cmaps/90ms-RKSJ-V.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"90msp-RKSJ-H",
        include_bytes!("cmaps/90msp-RKSJ-H.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"90msp-RKSJ-V",
        include_bytes!("cmaps/90msp-RKSJ-V.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"90pv-RKSJ-H",
        include_bytes!("cmaps/90pv-RKSJ-H.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"Add-RKSJ-H",
        include_bytes!("cmaps/Add-RKSJ-H.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"Add-RKSJ-V",
        include_bytes!("cmaps/Add-RKSJ-V.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"EUC-H",
        include_bytes!("cmaps/EUC-H.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"EUC-V",
        include_bytes!("cmaps/EUC-V.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"Ext-RKSJ-H",
        include_bytes!("cmaps/Ext-RKSJ-H.bin.z"),
        Ordering::Japan1,
    ),
    (
        b"Ext-RKSJ-V",
        include_bytes!("cmaps/Ext-RKSJ-V.bin.z"),
        Ordering::Japan1,
    ),
    (b"H", include_bytes!("cmaps/H.bin.z"), Ordering::Japan1),
    (b"V", include_bytes!("cmaps/V.bin.z"), Ordering::Japan1),
    (
        b"KSC-EUC-H",
        include_bytes!("cmaps/KSC-EUC-H.bin.z"),
        Ordering::Korea1,
    ),
    (
        b"KSC-EUC-V",
        include_bytes!("cmaps/KSC-EUC-V.bin.z"),
        Ordering::Korea1,
    ),
    (
        b"KSCms-UHC-H",
        include_bytes!("cmaps/KSCms-UHC-H.bin.z"),
        Ordering::Korea1,
    ),
    (
        b"KSCms-UHC-V",
        include_bytes!("cmaps/KSCms-UHC-V.bin.z"),
        Ordering::Korea1,
    ),
    (
        b"KSCms-UHC-HW-H",
        include_bytes!("cmaps/KSCms-UHC-HW-H.bin.z"),
        Ordering::Korea1,
    ),
    (
        b"KSCms-UHC-HW-V",
        include_bytes!("cmaps/KSCms-UHC-HW-V.bin.z"),
        Ordering::Korea1,
    ),
    (
        b"KSCpc-EUC-H",
        include_bytes!("cmaps/KSCpc-EUC-H.bin.z"),
        Ordering::Korea1,
    ),
];

static UNICODE_BLOBS: &[(Ordering, &[u8])] = &[
    (Ordering::GB1, include_bytes!("cmaps/Adobe-GB1-UCS2.bin.z")),
    (
        Ordering::CNS1,
        include_bytes!("cmaps/Adobe-CNS1-UCS2.bin.z"),
    ),
    (
        Ordering::Japan1,
        include_bytes!("cmaps/Adobe-Japan1-UCS2.bin.z"),
    ),
    (
        Ordering::Korea1,
        include_bytes!("cmaps/Adobe-Korea1-UCS2.bin.z"),
    ),
];

struct UniMap {
    /// (cid_lo, cid_hi, codepoint_start), sorted by cid_lo.
    ranges: Vec<(u32, u32, u32)>,
}

impl UniMap {
    fn lookup(&self, cid: u32) -> Option<char> {
        let i = self.ranges.partition_point(|r| r.0 <= cid);
        let r = self.ranges.get(i.checked_sub(1)?)?;
        if cid > r.1 {
            return None;
        }
        char::from_u32(r.2 + (cid - r.0))
    }
}

pub struct CjkCmap {
    /// (nbytes, lo, hi) codespace ranges.
    codespaces: Vec<(u32, u32, u32)>,
    /// (code_lo, code_hi, cid_start), sorted.
    cidranges: Vec<(u32, u32, u32)>,
    uni: Arc<UniMap>,
}

fn inflate_raw(z: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut dec = flate2::read::DeflateDecoder::new(z);
    dec.read_to_end(&mut out).expect("embedded cmap blob");
    out
}

fn read_triples(bin: &[u8], at: &mut usize) -> Vec<(u32, u32, u32)> {
    let n = u32::from_le_bytes(bin[*at..*at + 4].try_into().unwrap()) as usize;
    *at += 4;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let a = u32::from_le_bytes(bin[*at..*at + 4].try_into().unwrap());
        let b = u32::from_le_bytes(bin[*at + 4..*at + 8].try_into().unwrap());
        let c = u32::from_le_bytes(bin[*at + 8..*at + 12].try_into().unwrap());
        v.push((a, b, c));
        *at += 12;
    }
    v
}

fn unimap(ord: Ordering) -> Arc<UniMap> {
    static CACHE: OnceLock<[OnceLock<Arc<UniMap>>; 4]> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::array::from_fn(|_| OnceLock::new()));
    let idx = match ord {
        Ordering::GB1 => 0,
        Ordering::CNS1 => 1,
        Ordering::Japan1 => 2,
        Ordering::Korea1 => 3,
    };
    cache[idx]
        .get_or_init(|| {
            let blob = UNICODE_BLOBS.iter().find(|(o, _)| *o == ord).unwrap().1;
            let bin = inflate_raw(blob);
            let mut at = 0usize;
            Arc::new(UniMap {
                ranges: read_triples(&bin, &mut at),
            })
        })
        .clone()
}

/// CID -> unicode table for an Adobe ordering, addressed by the
/// CIDSystemInfo /Ordering name. For CID-keyed fonts without ToUnicode
/// the CID space itself is Adobe's, so the mapping applies no matter
/// which CMap encoded the codes.
#[derive(Clone)]
pub struct OrderingMap(Arc<UniMap>);

impl OrderingMap {
    pub fn lookup(&self, cid: u32) -> Option<char> {
        self.0.lookup(cid)
    }
}

pub fn ordering_map(ordering: &[u8]) -> Option<OrderingMap> {
    let ord = match ordering {
        b"GB1" => Ordering::GB1,
        b"CNS1" => Ordering::CNS1,
        b"Japan1" => Ordering::Japan1,
        b"Korea1" | b"KR" => Ordering::Korea1,
        _ => return None,
    };
    Some(OrderingMap(unimap(ord)))
}

/// Load a predefined CMap by name (cached per name).
pub fn lookup(name: &[u8]) -> Option<Arc<CjkCmap>> {
    static CACHE: OnceLock<std::sync::Mutex<rustc_hash::FxHashMap<Vec<u8>, Arc<CjkCmap>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(rustc_hash::FxHashMap::default()));
    if let Some(hit) = cache.lock().unwrap().get(name) {
        return Some(hit.clone());
    }
    let &(_, blob, ord) = ENCODINGS.iter().find(|(n, _, _)| *n == name)?;
    let bin = inflate_raw(blob);
    let mut at = 0usize;
    let codespaces = read_triples(&bin, &mut at);
    let cidranges = read_triples(&bin, &mut at);
    let cm = Arc::new(CjkCmap {
        codespaces,
        cidranges,
        uni: unimap(ord),
    });
    cache.lock().unwrap().insert(name.to_vec(), cm.clone());
    Some(cm)
}

/// Parse an embedded CMap stream: codespacerange + cidrange/cidchar
/// sections, plus an optional usecmap reference merged underneath.
pub fn parse_embedded(data: &[u8], uni: Option<&[u8]>) -> Option<Arc<CjkCmap>> {
    fn hex_between(s: &[u8], at: &mut usize) -> Option<u32> {
        while *at < s.len() && s[*at] != b'<' {
            if !s[*at].is_ascii_whitespace() {
                return None;
            }
            *at += 1;
        }
        let start = *at + 1;
        let end = start + s.get(start..)?.iter().position(|&b| b == b'>')?;
        let mut v = 0u32;
        for &b in &s[start..end] {
            v = (v << 4)
                | match b {
                    b'0'..=b'9' => (b - b'0') as u32,
                    b'a'..=b'f' => (b - b'a' + 10) as u32,
                    b'A'..=b'F' => (b - b'A' + 10) as u32,
                    _ => 0,
                };
        }
        *at = end + 1;
        Some(v)
    }
    // Byte length of the next hex token (counts digits, rounds up).
    fn hex_nbytes(s: &[u8], at: usize) -> u32 {
        let Some(open) = s[at..].iter().position(|&b| b == b'<') else {
            return 1;
        };
        let start = at + open + 1;
        let n = s[start..]
            .iter()
            .take_while(|&&b| b != b'>')
            .filter(|b| b.is_ascii_hexdigit())
            .count();
        (n as u32).div_ceil(2)
    }
    fn dec_at(s: &[u8], at: &mut usize) -> Option<u32> {
        while *at < s.len() && s[*at].is_ascii_whitespace() {
            *at += 1;
        }
        let start = *at;
        while *at < s.len() && s[*at].is_ascii_digit() {
            *at += 1;
        }
        std::str::from_utf8(&s[start..*at]).ok()?.parse().ok()
    }

    let mut codespaces: Vec<(u32, u32, u32)> = Vec::new();
    let mut cidranges: Vec<(u32, u32, u32)> = Vec::new();

    // usecmap: "/Name usecmap" pulls a predefined base underneath.
    let mut base: Option<Arc<CjkCmap>> = None;
    if let Some(u) = memchr::memmem::find(data, b" usecmap") {
        let head = &data[..u];
        if let Some(slash) = head.iter().rposition(|&b| b == b'/') {
            let name: Vec<u8> = head[slash + 1..]
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            base = lookup(&name);
        }
    }

    let sect = |open: &[u8], close: &[u8], three: bool, out: &mut Vec<(u32, u32, u32)>| {
        let mut pos = 0usize;
        while let Some(at) = memchr::memmem::find(&data[pos..], open) {
            let start = pos + at + open.len();
            let end =
                start + memchr::memmem::find(&data[start..], close).unwrap_or(data.len() - start);
            let body = &data[start..end];
            let mut p = 0usize;
            loop {
                if three {
                    let Some(lo) = hex_between(body, &mut p) else {
                        break;
                    };
                    let Some(hi) = hex_between(body, &mut p) else {
                        break;
                    };
                    let Some(cid) = dec_at(body, &mut p) else {
                        break;
                    };
                    out.push((lo, hi, cid));
                } else {
                    let Some(code) = hex_between(body, &mut p) else {
                        break;
                    };
                    let Some(cid) = dec_at(body, &mut p) else {
                        break;
                    };
                    out.push((code, code, cid));
                }
            }
            pos = end;
        }
    };
    sect(b"begincidrange", b"endcidrange", true, &mut cidranges);
    sect(b"begincidchar", b"endcidchar", false, &mut cidranges);

    // codespacerange (byte-length classes carried by digit count).
    let mut pos = 0usize;
    while let Some(at) = memchr::memmem::find(&data[pos..], b"begincodespacerange") {
        let start = pos + at + b"begincodespacerange".len();
        let end = start
            + memchr::memmem::find(&data[start..], b"endcodespacerange")
                .unwrap_or(data.len() - start);
        let body = &data[start..end];
        let mut p = 0usize;
        loop {
            let n = hex_nbytes(body, p);
            let Some(lo) = hex_between(body, &mut p) else {
                break;
            };
            let Some(hi) = hex_between(body, &mut p) else {
                break;
            };
            codespaces.push((n, lo, hi));
        }
        pos = end;
    }

    if let Some(b) = &base {
        codespaces.extend_from_slice(&b.codespaces);
        cidranges.extend_from_slice(&b.cidranges);
    }
    if codespaces.is_empty() && cidranges.is_empty() {
        return None;
    }
    if codespaces.is_empty() {
        codespaces.push((2, 0, 0xFFFF));
    }
    cidranges.sort_by_key(|r| r.0);
    let uni_map = uni
        .and_then(ordering_map)
        .map(|m| m.0)
        .unwrap_or_else(|| Arc::new(UniMap { ranges: Vec::new() }));
    Some(Arc::new(CjkCmap {
        codespaces,
        cidranges,
        uni: uni_map,
    }))
}

impl CjkCmap {
    /// How many bytes the code starting at data[0] occupies (codespace
    /// classification by leading byte; 1 when nothing matches).
    fn code_len(&self, data: &[u8]) -> usize {
        let b0 = data[0] as u32;
        for &(n, lo, hi) in &self.codespaces {
            let shift = (n - 1) * 8;
            if b0 >= (lo >> shift) && b0 <= (hi >> shift) {
                return (n as usize).min(data.len());
            }
        }
        1
    }

    fn cid(&self, code: u32) -> u32 {
        let i = self.cidranges.partition_point(|r| r.0 <= code);
        if let Some(r) = i.checked_sub(1).and_then(|i| self.cidranges.get(i)) {
            if code <= r.1 {
                return r.2 + (code - r.0);
            }
        }
        0
    }

    /// Parse the string bytes into (cid, unicode) pairs.
    pub fn decode(&self, bytes: &[u8]) -> Vec<(u32, Option<char>)> {
        let mut out = Vec::with_capacity(bytes.len() / 2 + 1);
        let mut i = 0usize;
        while i < bytes.len() {
            let n = self.code_len(&bytes[i..]);
            let mut code = 0u32;
            for &b in &bytes[i..i + n] {
                code = (code << 8) | b as u32;
            }
            i += n;
            let cid = self.cid(code);
            out.push((cid, self.uni.lookup(cid)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gbk_euc_h_decodes_ascii_and_hanzi() {
        let cm = lookup(b"GBK-EUC-H").expect("GBK-EUC-H embedded");
        // "A" (0x41) then U+4E00 一 (GBK 0xD2BB).
        let out = cm.decode(&[0x41, 0xD2, 0xBB]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].1, Some('A'));
        assert_eq!(out[1].1, Some('一'));
    }

    #[test]
    fn ninety_ms_rksj_decodes_shift_jis() {
        let cm = lookup(b"90ms-RKSJ-H").expect("90ms-RKSJ-H embedded");
        // Shift-JIS 0x93FA = 日, 0x967B = 本.
        let out = cm.decode(&[0x93, 0xFA, 0x96, 0x7B]);
        assert_eq!(out[0].1, Some('日'));
        assert_eq!(out[1].1, Some('本'));
    }

    #[test]
    fn ordering_map_japan1() {
        let m = ordering_map(b"Japan1").expect("Japan1 table");
        // Adobe-Japan1 CID 34 is 'A' (proportional Latin block starts at
        // CID 1 = space).
        assert_eq!(m.lookup(34), Some('A'));
        assert!(ordering_map(b"NoSuchOrdering").is_none());
    }

    #[test]
    fn unknown_name_is_none() {
        assert!(lookup(b"NoSuch-CMap").is_none());
    }
}
