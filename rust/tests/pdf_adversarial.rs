//! Adversarial invariants for the PDF object layer.
//!
//! A malformed PDF is normal input: object parsing and fast extraction
//! may return Err, but must never panic, hang, or read out of bounds.

use std::panic::{catch_unwind, AssertUnwindSafe};

use markit::{
    converters::pdf::{fast_extract::extract_pages_fast, index::PdfConverter, own_pdf::Pdf},
    types::{Converter, StreamInfo},
};

fn no_panic(data: &[u8]) {
    let parse = catch_unwind(AssertUnwindSafe(|| Pdf::parse(data)));
    assert!(parse.is_ok(), "Pdf::parse panicked on {} bytes", data.len());
    let extract = catch_unwind(AssertUnwindSafe(|| extract_pages_fast(data)));
    assert!(
        extract.is_ok(),
        "extract_pages_fast panicked on {} bytes",
        data.len()
    );
}

#[test]
fn structural_lies_never_panic() {
    let cases: &[&[u8]] = &[
        b"",
        b"%PDF-1.7",
        b"startxref
999999999999999999
%%EOF",
        b"xref
0 999999999
trailer << /Root 1 0 R >>
startxref
0",
        b"%PDF-1.4
xref
0 2
0000000000
startxref
9
%%EOF",
        b"1 0 obj << /Length -1 >> stream
x
endstream endobj",
        b"1 0 obj << /Length 999999999 >> stream
x",
        b"1 0 obj [1 2 3",                    // truncated array
        b"1 0 obj << /A << /B << /C 1 >> >>", // truncated nested dict
        br"1 0 obj (unterminated(literal",    // escaped/truncated string
        b"1 0 obj <ABC",                      // odd/truncated hex
        b"1 0 obj << /Filter /FlateDecode /Length 8 >> stream
not-zlib",
        b"%PDF-1.4
1 0 obj << /Type /Pages /Kids [1 0 R] >> endobj
trailer << /Root 1 0 R >>",
    ];
    for data in cases {
        no_panic(data);
    }
}

#[test]
fn full_conversion_rejects_malformed_image_dimensions_without_panicking() {
    let bytes = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >> endobj
4 0 obj << /Length 27 >> stream
q 10 0 0 10 0 0 cm /Im0 Do Q
endstream endobj
5 0 obj << /Type /XObject /Subtype /Image /Width 9223372036854775807 /Height 9223372036854775807 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Length 1 >> stream
x
endstream endobj
trailer << /Root 1 0 R >>
%%EOF";
    let image_dir =
        std::env::temp_dir().join(format!("markit-malformed-image-{}", std::process::id()));
    let info = StreamInfo {
        extension: Some(".pdf".into()),
        image_dir: Some(image_dir.to_string_lossy().into_owned()),
        ..StreamInfo::default()
    };
    let conversion = catch_unwind(AssertUnwindSafe(|| PdfConverter.convert(bytes, &info)));
    let _ = std::fs::remove_dir_all(image_dir);
    assert!(conversion.is_ok(), "full PDF conversion panicked");
}

#[test]
fn cyclic_reference_chain_is_rejected() {
    let bytes = b"%PDF-1.4
1 0 obj 1 0 R endobj
2 0 obj << /Type /Catalog /Pages 3 0 R >> endobj
3 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj
trailer << /Root 2 0 R >>";
    let pdf = Pdf::parse(bytes).expect("repair parse");
    let err = pdf.resolve(&markit::converters::pdf::own_pdf::Val::Ref(1));
    assert!(err.is_err(), "cyclic reference was silently returned");
}

#[test]
fn every_prefix_of_valid_pdf_is_safe() {
    let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj
trailer << /Root 1 0 R >>";
    for end in 0..=pdf.len() {
        no_panic(&pdf[..end]);
    }
}

#[test]
fn deterministic_mutations_never_panic() {
    let seed = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj
xref
0 3
0000000000 65535 f \n0000000009 00000 n \n0000000060 00000 n \ntrailer << /Size 3 /Root 1 0 R >>
startxref
116
%%EOF";
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..2_000 {
        let mut data = seed.to_vec();
        // xorshift64*: deterministic and dependency-free.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let r = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        match r % 4 {
            0 if !data.is_empty() => data.truncate((r as usize >> 8) % data.len()),
            1 if !data.is_empty() => {
                let at = (r as usize >> 8) % data.len();
                data[at] ^= (r >> 40) as u8;
            }
            2 => {
                let at = if data.is_empty() {
                    0
                } else {
                    (r as usize >> 8) % data.len()
                };
                data.insert(at, (r >> 32) as u8);
            }
            _ if !data.is_empty() => {
                let at = (r as usize >> 8) % data.len();
                data.remove(at);
            }
            _ => {}
        }
        no_panic(&data);
    }
}
