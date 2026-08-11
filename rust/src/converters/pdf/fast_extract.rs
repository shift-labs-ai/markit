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
) -> Result<(Interp<'a>, f64, f64)> {
    let media = inherited
        .media
        .as_ref()
        .ok_or_else(|| anyhow!("no MediaBox"))?;
    let media_bounds = [
        media[0].min(media[2]),
        media[1].min(media[3]),
        media[0].max(media[2]),
        media[1].max(media[3]),
    ];
    let page_box = if let Some(crop) = &inherited.crop {
        vec![
            crop[0].min(crop[2]).max(media_bounds[0]),
            crop[1].min(crop[3]).max(media_bounds[1]),
            crop[0].max(crop[2]).min(media_bounds[2]),
            crop[1].max(crop[3]).min(media_bounds[3]),
        ]
    } else {
        media_bounds.to_vec()
    };
    if page_box[2] <= page_box[0] || page_box[3] <= page_box[1] {
        bail!("empty page box");
    }
    let (base, page_width, page_height) =
        rotation_base(inherited.rotate, &page_box, page_box[0], page_box[1]);
    decode_page_content(pdf, page, content)?;
    let mut interp = Interp::new(pdf, page_number, base, hidden_ocgs);
    interp.run(content, inherited.resources.as_ref())?;
    interp.clip_to_page(page_width, page_height);
    Ok((interp, page_width, page_height))
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
    let mut any_decoded = false;
    let mut content = Vec::new();

    for (index, (page, inherited)) in pages.iter().enumerate() {
        let page_number = (index + 1) as u32;
        let (interp, page_width, page_height) = interpret_page(
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
        any_decoded |= interp.any_decoded;

        let raws = interp
            .items
            .into_iter()
            .map(|item| super::shared::RawTextItemPub {
                text: item.text,
                x: item.x,
                y: item.y,
                width: item.width,
                height: item.height,
                font_size: item.font_size,
                is_bold: item.is_bold,
            })
            .collect();
        let text_boxes = super::shared::finish_text_boxes_pub(raws, page_number)?;
        let bboxes: Vec<_> = interp
            .image_placements
            .iter()
            .map(|placement| device_bbox(placement.bbox, page_height))
            .collect();
        let images =
            super::shared::image_regions_from_bboxes_pub(&bboxes, page_number, page_height);
        out.push(PageContent {
            page_number,
            page_width,
            page_height,
            text_boxes,
            segments: interp.segments,
            images,
        });
    }

    // Text ops that never decoded a single glyph (not even whitespace)
    // mean the encoding is unsupported. Ops whose glyphs decoded to
    // blanks only are a genuinely empty overlay — e.g. a scanned page
    // with a whitespace text layer — and must not fail the document.
    if !any_text && any_text_ops && !any_decoded && !out.is_empty() {
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
    let (interp, _page_width, page_height) =
        interpret_page(pdf, page, inherited, page_number, hidden_ocgs, &mut content)?;
    Ok(interp
        .image_placements
        .into_iter()
        .filter_map(|placement| {
            let bbox = device_bbox(placement.bbox, page_height);
            super::shared::image_bbox_is_large_pub(bbox).then_some(placement.source)
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

    /// A page whose text ops decode only to whitespace is an image-only
    /// page, not an encoding failure.
    #[test]
    fn whitespace_only_text_ops_do_not_fail_extraction() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 41 >> stream
BT /F1 12 Tf 72 720 Td ( ) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("whitespace-only page");
        assert!(pages[0].text_boxes.is_empty());
    }

    /// A Type3 font whose /Differences names are opaque CharProc keys
    /// falls back to the base-encoding character for each code; a
    /// recognized name still wins.
    #[test]
    fn type3_opaque_differences_fall_back_to_base_encoding() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type3 /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << >> /Encoding << /Differences [65 /BQ 66 /eacute] >> /FirstChar 65 /LastChar 66 /Widths [500 500] >> endobj
5 0 obj << /Length 42 >> stream
BT /F1 12 Tf 72 720 Td (AB) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("type3 fallback");
        let text = text_of(&pages);
        assert!(text.contains("A\u{00E9}"), "got: {text}");
    }

    /// Adobe underscore ligature names in /Differences (`/T_h`, `/f_i`)
    /// must expand to their component letters — publishers like Springer
    /// map them with no usable ToUnicode entry.
    #[test]
    fn underscore_ligature_names_expand() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Minion /Encoding << /BaseEncoding /WinAnsiEncoding /Differences [30 /T_h 31 /f_i] >> >> endobj
5 0 obj << /Length 48 >> stream
BT /F1 12 Tf 72 720 Td (\\036e \\037rst) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("underscore ligatures");
        let text = text_of(&pages);
        assert!(text.contains("The first"), "got: {text}");
    }

    /// TeX Type3 fonts put f-ligatures at OT1 slots 11–15; the decode
    /// must expand them to ASCII so "first" doesn't lose its "fi".
    #[test]
    fn type3_tex_ligature_slots_expand_to_ascii() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type3 /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << >> /Encoding << /Differences [65 /BQ] >> /FirstChar 11 /LastChar 65 /Widths [500] >> endobj
5 0 obj << /Length 46 >> stream
BT /F1 12 Tf 72 720 Td (\\014rst) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).expect("tex ligature");
        let text = text_of(&pages);
        assert!(text.contains("first"), "got: {text}");
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
    /// according to tagged-PDF replacement semantics.
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
    fn crop_box_controls_page_geometry_and_clips_content() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /CropBox [0 0 300 400] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << >> stream
BT /F1 12 Tf 20 200 Td (inside) Tj 0 400 Td (outside) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).unwrap();
        assert_eq!(pages[0].page_width, 300.0);
        assert_eq!(pages[0].page_height, 400.0);
        let text = text_of(&pages);
        assert!(text.contains("inside"), "{text}");
        assert!(!text.contains("outside"), "{text}");
    }

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
    fn q_restores_font_text_state() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R /F2 6 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 62 >> stream
BT /F1 12 Tf 20 100 Td q /F2 12 Tf (A) Tj Q (A) Tj ET
endstream endobj
6 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Custom /Encoding << /Differences [65 /B] >> >> endobj
trailer << /Root 1 0 R >>";
        assert_eq!(text_of(&extract_pages_fast(pdf).unwrap()), "BA");
    }

    #[test]
    fn thick_strokes_are_not_table_segments() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >> endobj
4 0 obj << /Length 24 >> stream
20 w 0 0 m 100 0 l S
endstream endobj
trailer << /Root 1 0 R >>";
        assert!(extract_pages_fast(pdf).unwrap()[0].segments.is_empty());
    }

    #[test]
    fn sheared_image_uses_all_four_transformed_corners() {
        let pdf = br"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] /Resources << /XObject << /Im0 6 0 R >> >> /Contents 5 0 R >> endobj
5 0 obj << /Length 45 >> stream
q 100 100 -100 100 200 0 cm /Im0 Do Q
endstream endobj
6 0 obj << /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 1 /ColorSpace /DeviceGray /Length 1 >> stream
0
endstream endobj
trailer << /Root 1 0 R >>";
        let images = &extract_pages_fast(pdf).unwrap()[0].images;
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].bbox.w, 200.0);
        assert_eq!(images[0].bbox.h, 200.0);
    }

    #[test]
    fn form_bbox_clips_text_outside_its_bounds() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 4 0 R >> /XObject << /Fm 6 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << >> stream
q /Fm Do Q BT /F1 12 Tf 20 20 Td (visible) Tj ET
endstream endobj
6 0 obj << /Type /XObject /Subtype /Form /BBox [0 0 50 50] /Resources << /Font << /F1 4 0 R >> >> >> stream
BT /F1 12 Tf 100 100 Td (outside) Tj ET
endstream endobj
trailer << /Root 1 0 R >>";
        let text = text_of(&extract_pages_fast(pdf).unwrap());
        assert!(text.contains("visible"));
        assert!(!text.contains("outside"), "{text}");
    }

    #[test]
    fn form_level_hidden_ocg_is_suppressed() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R /OCProperties << /D << /OFF [7 0 R] >> >> >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 4 0 R >> /XObject << /Fm 6 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << >> stream
q /Fm Do Q BT /F1 12 Tf 20 20 Td (visible) Tj ET
endstream endobj
6 0 obj << /Type /XObject /Subtype /Form /BBox [0 0 200 200] /OC 7 0 R /Resources << /Font << /F1 4 0 R >> >> >> stream
BT /F1 12 Tf 20 100 Td (hidden) Tj ET
endstream endobj
7 0 obj << /Type /OCG >> endobj
trailer << /Root 1 0 R >>";
        let text = text_of(&extract_pages_fast(pdf).unwrap());
        assert!(text.contains("visible"));
        assert!(!text.contains("hidden"), "{text}");
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

    #[test]
    fn form_inside_hidden_marked_content_is_suppressed() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R /OCProperties << /D << /OFF [7 0 R] >> >> >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 4 0 R >> /XObject << /Fm 6 0 R >> /Properties << /MC0 7 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << >> stream
/OC /MC0 BDC /Fm Do EMC BT /F1 12 Tf 20 20 Td (visible) Tj ET
endstream endobj
6 0 obj << /Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> >> stream
BT /F1 12 Tf 20 100 Td (hidden) Tj ET
endstream endobj
7 0 obj << /Type /OCG >> endobj
trailer << /Root 1 0 R >>";
        let text = text_of(&extract_pages_fast(pdf).unwrap());
        assert!(text.contains("visible"));
        assert!(!text.contains("hidden"), "{text}");
    }

    #[test]
    fn inline_oc_membership_dictionary_suppresses_hidden_image() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R /OCProperties << /D << /OFF [7 0 R] >> >> >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R >> endobj
4 0 obj << /Length 180 >> stream
/OC << /OCGs [7 0 R] >> BDC
q 100 0 0 100 0 0 cm BI /W 1 /H 1 /BPC 1 /CS /G ID 0 EI Q
EMC
q 100 0 0 100 120 0 cm BI /W 1 /H 1 /BPC 1 /CS /G ID 0 EI Q
endstream endobj
7 0 obj << /Type /OCG >> endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).unwrap();
        assert_eq!(pages[0].images.len(), 1);
        assert_eq!(pages[0].images[0].bbox.x, 120.0);
    }

    #[test]
    fn actual_text_geometry_unions_all_suppressed_runs() {
        let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 130 >> stream
BT /F1 12 Tf /Span << /ActualText (replacement) >> BDC 72 720 Td (wrong1) Tj 0 -100 Td (wrong2) Tj EMC ET
endstream endobj
trailer << /Root 1 0 R >>";
        let pages = extract_pages_fast(pdf).unwrap();
        let item = pages[0]
            .text_boxes
            .iter()
            .find(|item| item.text.contains("replacement"))
            .unwrap();
        assert!(
            item.bounds.bottom < 650.0,
            "bottom must include second run: {}",
            item.bounds.bottom
        );
        assert!(
            item.bounds.top - item.bounds.bottom > 100.0,
            "bounds must span both runs: {}..{}",
            item.bounds.bottom,
            item.bounds.top
        );
    }
}
