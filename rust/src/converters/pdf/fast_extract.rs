//! markit's PDF extraction engine: a pure-Rust content-stream
//! interpreter over the own_pdf object layer, producing the same
//! PageContent shape the MuPDF path produces at a fraction of the cost.
//! MuPDF remains the rasterizer (render_image_region) and the fallback
//! for anything this engine cannot handle faithfully (see extract_pages).
//!
//! Coordinates: PDF user space is bottom-left/y-up, which is what the
//! downstream pipeline consumes for text boxes and segments. Image
//! regions keep the MuPDF path's device-space (y-down) convention.

use anyhow::{anyhow, bail, Result};

use super::geom::rotation_base;
use super::interp::Interp;
use super::own_pdf::{decode_stream, Dict, Pdf, Val};
use super::pagetree::{collect_hidden_ocgs, walk_pages, Inherit};
use super::types::PageContent;

/// Extract all pages via the own object layer. Errors mean "use the
/// MuPDF fallback" (encryption, non-Flate filters, rotated pages,
/// zero-text documents, structural surprises).
pub fn extract_pages_fast(input: &[u8]) -> Result<Vec<PageContent>> {
    let pdf = Pdf::parse(input)?;

    // Page tree walk (Root → Pages → Kids), tracking inheritable attrs.
    let Some(Val::Dict(root)) = pdf.dict_get(&pdf.trailer, b"Root")? else {
        bail!("no Root");
    };
    let hidden_ocgs = collect_hidden_ocgs(&pdf, &root);
    let Some(Val::Dict(pages_root)) = pdf.dict_get(&root, b"Pages")? else {
        bail!("no Pages");
    };

    let mut page_dicts = Vec::new();
    walk_pages(&pdf, &pages_root, &Inherit::default(), &mut page_dicts, 0)?;

    let mut out: Vec<PageContent> = Vec::with_capacity(page_dicts.len());
    let mut any_text = false;
    let mut any_text_ops = false;
    let mut content_buf: Vec<u8> = Vec::new();

    for (idx, (page, inh)) in page_dicts.iter().enumerate() {
        let page_no = (idx + 1) as u32;

        let mb = inh.media.as_ref().ok_or_else(|| anyhow!("no MediaBox"))?;
        let (mx0, my0) = (mb[0].min(mb[2]), mb[1].min(mb[3]));
        let (base, page_height) = rotation_base(inh.rotate, mb, mx0, my0);

        // Concatenate content streams.
        content_buf.clear();
        match pdf.dict_get(page, b"Contents")? {
            Some(Val::Stream(d, raw)) => {
                content_buf.extend_from_slice(&decode_stream(&d, raw, &pdf)?);
            }
            Some(Val::Array(items)) => {
                for it in items {
                    if let Val::Stream(d, raw) = pdf.resolve(&it)? {
                        content_buf.extend_from_slice(&decode_stream(&d, raw, &pdf)?);
                        content_buf.push(b'\n');
                    }
                }
            }
            _ => {}
        }

        let mut interp = Interp::new(&pdf, page_no, base, hidden_ocgs.clone());
        interp.run(&content_buf, inh.resources.as_ref())?;

        if !interp.items.is_empty() {
            any_text = true;
        }
        if interp.text_ops > 0 {
            any_text_ops = true;
        }
        if interp.unsupported_font {
            bail!("unsupported predefined CMap (CJK encoding tables)");
        }

        // Text boxes through the shared merge pipeline (items are already
        // in bottom-left user space).
        let raws: Vec<super::extract::RawTextItemPub> = interp
            .items
            .into_iter()
            .map(|i| super::extract::RawTextItemPub {
                text: i.text,
                x: i.x,
                y: i.y,
                width: i.width,
                height: i.height,
                font_size: i.font_size,
                is_bold: i.is_bold,
            })
            .collect();
        let text_boxes = super::extract::finish_text_boxes_pub(raws, page_no)?;

        // Image regions: convert user-space bbox to the device-space
        // convention image_regions expects (y down, int truncation).
        let bboxes: Vec<(f32, f32, f32, f32)> = interp
            .image_bboxes
            .iter()
            .map(|&(x0, y0, x1, y1)| {
                (
                    x0 as f32,
                    (page_height - y1) as f32,
                    x1 as f32,
                    (page_height - y0) as f32,
                )
            })
            .collect();
        let images = super::extract::image_regions_from_bboxes_pub(&bboxes, page_no, page_height);

        out.push(PageContent {
            page_number: page_no,
            text_boxes,
            segments: interp.segments,
            images,
        });
    }

    // Text operators that produced nothing = an encoding we failed to
    // decode: defer to the fallback. No text operators at all = a scanned
    // document: image placeholders are the right output, same as MuPDF.
    if !any_text && any_text_ops && !out.is_empty() {
        bail!("text ops decoded to nothing (unsupported encodings)");
    }

    Ok(out)
}
/// Image placements for one page, in the same order and with the same
/// area filter as the ImageRegion ids assigned during extraction
/// ("p{page}-img{i}"), so a region id indexes directly into this list.
pub(crate) fn page_image_placements<'a>(
    pdf: &'a Pdf<'a>,
    page_number: u32,
) -> Result<Vec<(Dict<'a>, &'a [u8])>> {
    let Some(Val::Dict(root)) = pdf.dict_get(&pdf.trailer, b"Root")? else {
        bail!("no Root");
    };
    let hidden_ocgs = collect_hidden_ocgs(pdf, &root);
    let Some(Val::Dict(pages_root)) = pdf.dict_get(&root, b"Pages")? else {
        bail!("no Pages");
    };
    let mut page_dicts = Vec::new();
    walk_pages(pdf, &pages_root, &Inherit::default(), &mut page_dicts, 0)?;
    let Some((page, inh)) = page_dicts.into_iter().nth(page_number as usize - 1) else {
        bail!("page out of range");
    };

    let mb = inh.media.as_ref().ok_or_else(|| anyhow!("no MediaBox"))?;
    let (mx0, my0) = (mb[0].min(mb[2]), mb[1].min(mb[3]));
    let (base, page_height) = rotation_base(inh.rotate, mb, mx0, my0);

    let mut content_buf: Vec<u8> = Vec::new();
    match pdf.dict_get(&page, b"Contents")? {
        Some(Val::Stream(d, raw)) => {
            content_buf.extend_from_slice(&decode_stream(&d, raw, pdf)?);
        }
        Some(Val::Array(items)) => {
            for it in items {
                if let Val::Stream(d, raw) = pdf.resolve(&it)? {
                    content_buf.extend_from_slice(&decode_stream(&d, raw, pdf)?);
                    content_buf.push(b'\n');
                }
            }
        }
        _ => {}
    }

    let mut interp = Interp::new(pdf, page_number, base, hidden_ocgs.clone());
    interp.run(&content_buf, inh.resources.as_ref())?;

    // Apply the same MIN_IMAGE_AREA filter (int-truncated device coords)
    // that assigned the region ids.
    let mut out = Vec::new();
    for (i, &(x0, y0, x1, y1)) in interp.image_bboxes.iter().enumerate() {
        let dev = (
            x0 as f32,
            (page_height - y1) as f32,
            x1 as f32,
            (page_height - y0) as f32,
        );
        let w = ((dev.2 - dev.0) as i32) as f64;
        let h = ((dev.3 - dev.1) as i32) as f64;
        if w * h < super::extract::MIN_IMAGE_AREA_PUB {
            continue;
        }
        out.push(interp.image_xobjects[i].clone());
    }
    Ok(out)
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
}
