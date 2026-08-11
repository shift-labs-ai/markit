//! markit's pure-Rust PDF extraction engine.
//!
//! One shared page interpreter feeds both markdown extraction and native
//! image-source lookup. Coordinates remain PDF user space (bottom-left,
//! Y-up) until image regions are converted to device space.

use std::rc::Rc;

use anyhow::{anyhow, bail, Result};
use rustc_hash::FxHashSet;

use super::geom::rotation_base;
use super::interp::{ImageSource, Interp};
use super::own_pdf::{decode_stream, Dict, Pdf, Val};
use super::pagetree::{collect_hidden_ocgs, walk_pages, Inherit};
use super::types::PageContent;

type PageNode<'a> = (Dict<'a>, Inherit<'a>);

fn page_tree<'a>(pdf: &'a Pdf<'a>) -> Result<(Rc<FxHashSet<u32>>, Vec<PageNode<'a>>)> {
    let Some(Val::Dict(root)) = pdf.dict_get(&pdf.trailer, b"Root")? else {
        bail!("no Root");
    };
    let hidden_ocgs = collect_hidden_ocgs(pdf, &root);
    let Some(Val::Dict(pages_root)) = pdf.dict_get(&root, b"Pages")? else {
        bail!("no Pages");
    };
    let mut pages = Vec::new();
    walk_pages(pdf, &pages_root, &Inherit::default(), &mut pages, 0)?;
    Ok((hidden_ocgs, pages))
}

fn decode_page_content(pdf: &Pdf<'_>, page: &Dict<'_>, content: &mut Vec<u8>) -> Result<()> {
    content.clear();
    match pdf.dict_get(page, b"Contents")? {
        Some(Val::Stream(dict, raw)) => {
            content.extend_from_slice(&decode_stream(&dict, raw, pdf)?);
        }
        Some(Val::Array(items)) => {
            for item in items {
                if let Val::Stream(dict, raw) = pdf.resolve(&item)? {
                    content.extend_from_slice(&decode_stream(&dict, raw, pdf)?);
                    content.push(b'\n');
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn interpret_page<'a>(
    pdf: &'a Pdf<'a>,
    page: &Dict<'a>,
    inherited: &Inherit<'a>,
    page_number: u32,
    hidden_ocgs: Rc<FxHashSet<u32>>,
    content: &mut Vec<u8>,
) -> Result<(Interp<'a>, f64)> {
    let media = inherited
        .media
        .as_ref()
        .ok_or_else(|| anyhow!("no MediaBox"))?;
    let (mx0, my0) = (media[0].min(media[2]), media[1].min(media[3]));
    let (base, page_height) = rotation_base(inherited.rotate, media, mx0, my0);
    decode_page_content(pdf, page, content)?;
    let mut interp = Interp::new(pdf, page_number, base, hidden_ocgs);
    interp.run(content, inherited.resources.as_ref())?;
    Ok((interp, page_height))
}

fn device_bbox((x0, y0, x1, y1): (f64, f64, f64, f64), page_height: f64) -> (f32, f32, f32, f32) {
    (
        x0 as f32,
        (page_height - y1) as f32,
        x1 as f32,
        (page_height - y0) as f32,
    )
}

pub fn extract_pages_fast(input: &[u8]) -> Result<Vec<PageContent>> {
    let pdf = Pdf::parse(input)?;
    let (hidden_ocgs, pages) = page_tree(&pdf)?;
    let mut out = Vec::with_capacity(pages.len());
    let mut any_text = false;
    let mut any_text_ops = false;
    let mut content = Vec::new();

    for (index, (page, inherited)) in pages.iter().enumerate() {
        let page_number = (index + 1) as u32;
        let (interp, page_height) = interpret_page(
            &pdf,
            page,
            inherited,
            page_number,
            hidden_ocgs.clone(),
            &mut content,
        )?;
        if interp.unsupported_font {
            bail!("unsupported predefined CMap (CJK encoding tables)");
        }
        any_text |= !interp.items.is_empty();
        any_text_ops |= interp.text_ops > 0;

        let raws = interp
            .items
            .into_iter()
            .map(|item| super::extract::RawTextItemPub {
                text: item.text,
                x: item.x,
                y: item.y,
                width: item.width,
                height: item.height,
                font_size: item.font_size,
                is_bold: item.is_bold,
            })
            .collect();
        let text_boxes = super::extract::finish_text_boxes_pub(raws, page_number)?;
        let bboxes: Vec<_> = interp
            .image_placements
            .iter()
            .map(|placement| device_bbox(placement.bbox, page_height))
            .collect();
        let images =
            super::extract::image_regions_from_bboxes_pub(&bboxes, page_number, page_height);
        out.push(PageContent {
            page_number,
            text_boxes,
            segments: interp.segments,
            images,
        });
    }

    if !any_text && any_text_ops && !out.is_empty() {
        bail!("text ops decoded to nothing (unsupported encodings)");
    }
    Ok(out)
}

pub(crate) fn page_image_placements<'a>(
    pdf: &'a Pdf<'a>,
    page_number: u32,
) -> Result<Vec<ImageSource<'a>>> {
    let (hidden_ocgs, pages) = page_tree(pdf)?;
    let Some((page, inherited)) = pages.get(page_number as usize - 1) else {
        bail!("page out of range");
    };
    let mut content = Vec::new();
    let (interp, page_height) =
        interpret_page(pdf, page, inherited, page_number, hidden_ocgs, &mut content)?;
    Ok(interp
        .image_placements
        .into_iter()
        .filter_map(|placement| {
            let bbox = device_bbox(placement.bbox, page_height);
            super::extract::image_bbox_is_large_pub(bbox).then_some(placement.source)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Option<Vec<u8>> {
        let path = format!("../test/fixtures/pdfs/encrypted/{name}");
        std::fs::read(path).ok()
    }

    fn text_of(pages: &[PageContent]) -> String {
        pages
            .iter()
            .flat_map(|p| p.text_boxes.iter())
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Every empty-password encryption revision must decrypt to exactly
    /// the plaintext document's extraction. Fixtures generated with qpdf
    /// from our own t.pdf (see test/fixtures/pdfs/encrypted/).
    #[test]
    fn encrypted_variants_match_plaintext() {
        let Some(plain) = fixture("plain.pdf") else {
            eprintln!("Skipping: encrypted fixtures not found");
            return;
        };
        let expect = text_of(&extract_pages_fast(&plain).unwrap());
        assert!(!expect.is_empty());

        for name in ["rc4-40.pdf", "rc4-128.pdf", "aesv2.pdf", "aes256.pdf"] {
            let bytes = fixture(name).unwrap();
            let pages = extract_pages_fast(&bytes)
                .unwrap_or_else(|e| panic!("{name}: fast path failed: {e}"));
            assert_eq!(text_of(&pages), expect, "{name} extraction differs");
        }

        // Password routes share this test because MARKIT_PDF_PASSWORD is
        // process-global: user and owner passwords both decrypt, a wrong
        // password is refused.
        for name in ["pw-aes256.pdf", "pw-rc4-128.pdf"] {
            let bytes = fixture(name).unwrap();
            for pw in ["usr", "own"] {
                std::env::set_var("MARKIT_PDF_PASSWORD", pw);
                let pages =
                    extract_pages_fast(&bytes).unwrap_or_else(|e| panic!("{name}/{pw}: {e}"));
                assert_eq!(text_of(&pages), expect, "{name}/{pw}");
            }
            std::env::set_var("MARKIT_PDF_PASSWORD", "wrong");
            assert!(
                extract_pages_fast(&bytes).is_err(),
                "{name} accepted a wrong password"
            );
            std::env::remove_var("MARKIT_PDF_PASSWORD");
        }
    }

    /// Password-protected fixtures decrypt with either the user or the
    /// owner password and refuse a wrong one. Env-var scoped in a single
    /// A Type1 font with no ToUnicode and no /Encoding must decode
    /// through its embedded program's cleartext encoding vector. This
    /// is the regression test for the recovery chain that was once
    /// committed but never wired into build_font.
    #[test]
    fn type1_fontfile_encoding_recovery() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /QQQQQQ+Custom /FontDescriptor 6 0 R >> endobj
5 0 obj << /Length 44 >> stream
BT /F1 12 Tf 72 720 Td (AB) Tj ET
endstream endobj
6 0 obj << /Type /FontDescriptor /FontName /QQQQQQ+Custom /Flags 4 /FontFile 7 0 R >> endobj
7 0 obj << /Length 150 >> stream
%!PS-AdobeFont-1.0: Custom
/Encoding 256 array
0 1 255 {1 index exch /.notdef put} for
dup 65 /eacute put
dup 66 /oslash put
readonly def
eexec
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("type1 recovery");
        let text = text_of(&pages);
        assert!(text.contains("\u{00E9}\u{00F8}"), "got: {text}");
    }

    /// Content inside an /OC span whose OCG is OFF in the default
    /// configuration is invisible and must be suppressed.
    #[test]
    fn hidden_ocg_layer_suppressed() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [7 0 R] /D << /OFF [7 0 R] >> >> >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> /Properties << /MC0 7 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 92 >> stream
BT /F1 12 Tf 72 720 Td (visible) Tj /OC /MC0 BDC ( hidden) Tj EMC ( also-visible) Tj ET
endstream endobj
7 0 obj << /Type /OCG /Name (Watermark) >> endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("ocg");
        let text = text_of(&pages);
        assert!(text.contains("visible"), "got: {text}");
        assert!(!text.contains("hidden"), "got: {text}");
        assert!(text.contains("also-visible"), "got: {text}");
    }

    /// /ActualText in a marked-content span replaces the drawn glyphs
    /// (tagged-PDF semantics, same as MuPDF).
    #[test]
    fn actual_text_replaces_glyphs() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 96 >> stream
BT /F1 12 Tf 72 720 Td /Span << /ActualText (correct) >> BDC (wrong) Tj EMC ( after) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("actualtext");
        let text = text_of(&pages);
        assert!(text.contains("correct"), "got: {text}");
        assert!(!text.contains("wrong"), "got: {text}");
        assert!(text.contains("after"), "got: {text}");
    }

    /// A predefined CJK CMap (GBK-EUC-H) decodes both the multi-byte
    /// hanzi codespace and 1-byte ASCII through Adobe's tables.
    #[test]
    fn predefined_cjk_cmap_decodes() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 6 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /GBK-EUC-H /DescendantFonts [5 0 R] >> endobj
5 0 obj << /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >> /DW 1000 >> endobj
6 0 obj << /Length 52 >> stream
BT /F1 12 Tf 72 720 Td <C4E3BAC3> Tj (Hi) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("cjk cmap");
        let text = text_of(&pages);
        assert!(text.contains("\u{4F60}\u{597D}"), "got: {text}");
        assert!(text.contains("Hi"), "got: {text}");
    }

    /// A wrong /Length must not corrupt the stream: the parser verifies
    /// the endstream delimiter and recovers by scanning.
    #[test]
    fn wrong_stream_length_recovers() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 9999 >> stream
BT /F1 12 Tf 72 720 Td (Recovered) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("length repair");
        assert!(text_of(&pages).contains("Recovered"));
    }

    /// A rotated page must still extract its text (geometry transformed,
    /// not rejected).
    #[test]
    fn rotated_page_extracts() {
        // Minimal uncompressed PDF, /Rotate 90.
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 44 >> stream
BT /F1 12 Tf 72 720 Td (Hello rotated) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        // No xref: exercises the repair scan too.
        let pages = extract_pages_fast(pdf).expect("rotated page");
        let text = text_of(&pages);
        assert!(text.contains("Hello rotated"), "got: {text}");
        // 90° rotation swaps visual dimensions: the text box must sit
        // within the rotated page's width (the original height).
        let tb = &pages[0].text_boxes[0];
        assert!(tb.bounds.left >= 0.0 && tb.bounds.right <= 792.0);
    }

    #[test]
    fn base_encoding_dict_without_differences_remains_authoritative() {
        let pdf = br"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Custom /Encoding << /BaseEncoding /MacRomanEncoding >> /FontDescriptor 6 0 R >> endobj
5 0 obj << /Length 38 >> stream
BT /F1 12 Tf 72 720 Td (\200) Tj ET
endstream endobj
6 0 obj << /Type /FontDescriptor /FontName /Custom /Flags 4 /FontFile 7 0 R >> endobj
7 0 obj << /Length 100 >> stream
%!PS
/Encoding 256 array
dup 128 /A put
readonly def
eexec
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).unwrap();
        assert!(text_of(&pages).contains('Ä'));
        assert!(!text_of(&pages).contains('A'));
    }

    #[test]
    fn unsupported_mac_expert_encoding_refuses_silent_winansi_garbage() {
        let pdf = br"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Custom /Encoding /MacExpertEncoding >> endobj
5 0 obj << /Length 38 >> stream
BT /F1 12 Tf 72 720 Td (\200) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        assert!(extract_pages_fast(pdf).is_err());
    }

    #[test]
    fn inline_and_xobject_image_sources_stay_aligned_with_regions() {
        let pdf = br"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /XObject << /Im0 6 0 R >> >> /Contents 5 0 R >> endobj
5 0 obj << /Length 120 >> stream
q 100 0 0 100 0 0 cm BI /W 1 /H 1 /BPC 1 /CS /G ID 0 EI Q
q 100 0 0 100 120 0 cm /Im0 Do Q
endstream endobj
6 0 obj << /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 1 /ColorSpace /DeviceGray /Length 1 >> stream
0
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).unwrap();
        assert_eq!(pages[0].images.len(), 2);
        assert_eq!(pages[0].images[0].id, "p1-img0");
        assert_eq!(pages[0].images[1].id, "p1-img1");

        let parsed = Pdf::parse(pdf).unwrap();
        let placements = page_image_placements(&parsed, 1).unwrap();
        assert_eq!(placements.len(), 2);
        assert!(matches!(placements[0], ImageSource::Inline));
        assert!(matches!(
            placements[1],
            ImageSource::XObject { raw: b"0", .. }
        ));
    }

    #[test]
    fn own_interpreter_close_paint_adds_implicit_edge() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >> endobj
4 0 obj << /Length 40 >> stream
0 0 m 100 0 l 100 100 l 0 100 l b
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).unwrap();
        assert_eq!(pages[0].segments.len(), 4);
    }

    #[test]
    fn own_interpreter_fill_stroke_thin_rectangle_is_one_rule() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R >> endobj
4 0 obj << /Length 17 >> stream
0 0 200 1 re B
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).unwrap();
        assert_eq!(pages[0].segments.len(), 1);
    }
}
