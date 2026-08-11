//! Embedded-image extraction from PDF pages — the pure-Rust replacement
//! for MuPDF region rasterization. Instead of rendering a page crop, the
//! placed image XObject itself is extracted at native resolution:
//! DCTDecode streams pass through as JPEG files, JPXDecode as JP2;
//! CCITT decodes to grayscale PNG; FlateDecode (and raw) bitmaps
//! re-encode as PNG; JBIG2 decodes through hayro-jbig2 (pure Rust).

use anyhow::{anyhow, bail, Result};

use super::interp::ImageSource;
use super::own_pdf::{decode_stream, Dict, Pdf, Val};
use super::types::ImageRegion;

/// Extracted image bytes plus the file extension they should carry.
pub struct ExtractedImage {
    pub bytes: Vec<u8>,
    pub ext: &'static str,
}

/// Extract the image behind an ImageRegion. The region id encodes the
/// per-page ordinal ("p{page}-img{i}") assigned over the same
/// area-filtered placement order this function reproduces.
pub fn extract_image_region_fast(input: &[u8], region: &ImageRegion) -> Result<ExtractedImage> {
    let pdf = Pdf::parse(input)?;
    let placements = super::fast_extract::page_image_placements(&pdf, region.page_number)?;

    let idx: usize = region
        .id
        .rsplit("img")
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("bad region id"))?;
    let source = placements
        .into_iter()
        .nth(idx)
        .ok_or_else(|| anyhow!("image index out of range"))?;
    match source {
        ImageSource::Inline => bail!("inline image: no extractable stream"),
        ImageSource::XObject { dict, raw } => extract_xobject(&pdf, &dict, raw),
    }
}

/// JBIG2Decode → grayscale PNG (pure-Rust hayro-jbig2).
fn extract_jbig2(data: &[u8], globals: Option<&[u8]>) -> Result<ExtractedImage> {
    struct Sink {
        rows: Vec<u8>,
        width: usize,
        row: Vec<u8>,
    }
    impl hayro_jbig2::Decoder for Sink {
        fn push_pixel(&mut self, black: bool) {
            self.row.push(if black { 0 } else { 255 });
        }
        fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
            let v = if black { 0 } else { 255 };
            self.row
                .extend(std::iter::repeat_n(v, chunk_count as usize * 8));
        }
        fn next_line(&mut self) {
            self.row.resize(self.width, 255);
            self.rows.extend_from_slice(&self.row);
            self.row.clear();
        }
    }

    let img =
        hayro_jbig2::Image::new_embedded(data, globals).map_err(|e| anyhow!("jbig2: {e:?}"))?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 || w.saturating_mul(h) > 100_000_000 {
        bail!("jbig2: bad dimensions");
    }
    let mut sink = Sink {
        rows: Vec::with_capacity(w * h),
        width: w,
        row: Vec::with_capacity(w),
    };
    img.decode(&mut sink).map_err(|e| anyhow!("jbig2: {e:?}"))?;
    sink.rows.resize(w * h, 255);

    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, w as u32, h as u32);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header()?;
        wr.write_image_data(&sink.rows)?;
    }
    Ok(ExtractedImage {
        bytes: png,
        ext: "png",
    })
}

/// CCITTFaxDecode → grayscale PNG.
fn extract_ccitt(pdf: &Pdf, dict: &Dict, data: &[u8]) -> Result<ExtractedImage> {
    let width = pdf
        .dict_get(dict, b"Width")?
        .and_then(|v| v.as_num())
        .ok_or_else(|| anyhow!("no Width"))? as usize;
    let height = pdf
        .dict_get(dict, b"Height")?
        .and_then(|v| v.as_num())
        .unwrap_or(0.0) as usize;

    let parms = match pdf.dict_get(dict, b"DecodeParms")? {
        Some(Val::Dict(d)) => Some(d),
        Some(Val::Array(a)) => a.iter().find_map(|v| match pdf.resolve(v) {
            Ok(Val::Dict(d)) => Some(d),
            _ => None,
        }),
        _ => None,
    };
    let pg = |key: &[u8], dflt: f64| -> f64 {
        parms
            .as_ref()
            .and_then(|d| pdf.dict_get(d, key).ok().flatten())
            .and_then(|v| v.as_num())
            .unwrap_or(dflt)
    };
    let k = pg(b"K", 0.0) as i32;
    // PDF image XObjects default Columns to their /Width. The 1728
    // default belongs to standalone fax data, not an image dictionary.
    let cols = parms
        .as_ref()
        .and_then(|d| pdf.dict_get(d, b"Columns").ok().flatten())
        .and_then(|v| v.as_num())
        .map(|v| v as usize)
        .filter(|&v| v > 0)
        .unwrap_or(width);
    let byte_align = matches!(
        parms
            .as_ref()
            .and_then(|d| pdf.dict_get(d, b"EncodedByteAlign").ok().flatten()),
        Some(Val::Bool(true))
    );
    let black_is_1 = matches!(
        parms
            .as_ref()
            .and_then(|d| pdf.dict_get(d, b"BlackIs1").ok().flatten()),
        Some(Val::Bool(true))
    );

    let gray = super::ccitt::decode(data, k, cols, height, byte_align, black_is_1)?;
    let rows = gray.len() / cols;
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, cols as u32, rows as u32);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header()?;
        w.write_image_data(&gray)?;
    }
    Ok(ExtractedImage {
        bytes: png,
        ext: "png",
    })
}

fn jbig2_globals(pdf: &Pdf, dict: &Dict, filter_index: usize) -> Option<Vec<u8>> {
    let parms = match pdf.dict_get(dict, b"DecodeParms").ok()?? {
        Val::Dict(d) => Some(d),
        Val::Array(a) => match a.get(filter_index).map(|v| pdf.resolve(v)) {
            Some(Ok(Val::Dict(d))) => Some(d),
            _ => None,
        },
        _ => None,
    }?;
    match pdf.dict_get(&parms, b"JBIG2Globals").ok()?? {
        Val::Stream(gd, raw) => decode_stream(&gd, raw, pdf).ok(),
        _ => None,
    }
}

fn extract_xobject(pdf: &Pdf, dict: &Dict, raw: &[u8]) -> Result<ExtractedImage> {
    // Find the outermost filter; DCT passes through undecoded.
    let filter_names: Vec<Vec<u8>> = match pdf.dict_get(dict, b"Filter")? {
        Some(Val::Name(n)) => vec![n.to_vec()],
        Some(Val::Array(a)) => a
            .iter()
            .filter_map(|v| pdf.resolve(v).ok())
            .filter_map(|v| v.as_name().map(|n| n.to_vec()))
            .collect(),
        _ => Vec::new(),
    };

    if filter_names.last().map(|n| n.as_slice()) == Some(b"JBIG2Decode") {
        let mut bytes = pdf.decrypt_stream_pub(raw)?;
        for f in &filter_names[..filter_names.len() - 1] {
            if f.as_slice() == b"FlateDecode" {
                bytes = super::own_pdf::inflate_pub(&bytes)?;
            } else {
                bail!("unsupported pre-JBIG2 filter");
            }
        }
        let globals = jbig2_globals(pdf, dict, filter_names.len() - 1);
        return extract_jbig2(&bytes, globals.as_deref());
    }

    if filter_names.last().map(|n| n.as_slice()) == Some(b"JPXDecode") {
        // A JPXDecode payload is a complete JP2/J2K codestream.
        let mut bytes = pdf.decrypt_stream_pub(raw)?;
        for f in &filter_names[..filter_names.len() - 1] {
            if f.as_slice() == b"FlateDecode" {
                bytes = super::own_pdf::inflate_pub(&bytes)?;
            } else {
                bail!("unsupported pre-JPX filter");
            }
        }
        return Ok(ExtractedImage { bytes, ext: "jp2" });
    }

    if filter_names.last().map(|n| n.as_slice()) == Some(b"CCITTFaxDecode") {
        let mut bytes = pdf.decrypt_stream_pub(raw)?;
        for f in &filter_names[..filter_names.len() - 1] {
            if f.as_slice() == b"FlateDecode" {
                bytes = super::own_pdf::inflate_pub(&bytes)?;
            } else {
                bail!("unsupported pre-CCITT filter");
            }
        }
        return extract_ccitt(pdf, dict, &bytes);
    }

    if filter_names.last().map(|n| n.as_slice()) == Some(b"DCTDecode") {
        // The (possibly Flate-wrapped) payload is a complete JPEG file.
        let mut bytes = pdf.decrypt_stream_pub(raw)?;
        for f in &filter_names[..filter_names.len() - 1] {
            if f.as_slice() == b"FlateDecode" {
                bytes = super::own_pdf::inflate_pub(&bytes)?;
            } else {
                bail!("unsupported pre-DCT filter");
            }
        }
        return Ok(ExtractedImage { bytes, ext: "jpg" });
    }

    // Bitmap path: fully decode, then PNG-encode.
    let data = decode_stream(dict, raw, pdf)?;
    let width = pdf
        .dict_get(dict, b"Width")?
        .and_then(|v| v.as_num())
        .ok_or_else(|| anyhow!("no Width"))? as u32;
    let height = pdf
        .dict_get(dict, b"Height")?
        .and_then(|v| v.as_num())
        .ok_or_else(|| anyhow!("no Height"))? as u32;
    let bpc = pdf
        .dict_get(dict, b"BitsPerComponent")?
        .and_then(|v| v.as_num())
        .unwrap_or(8.0) as u32;

    let (components, palette, tint_inverted) = color_info(pdf, dict)?;
    let row_in = (width as usize * components * bpc as usize).div_ceil(8);
    if data.len() < row_in * height as usize {
        bail!("short image data");
    }

    // Normalize to 8-bit samples.
    let mut samples: Vec<u8> = Vec::with_capacity(width as usize * height as usize * components);
    for row in 0..height as usize {
        let r = &data[row * row_in..(row + 1) * row_in];
        match bpc {
            8 => samples.extend_from_slice(&r[..width as usize * components]),
            1 | 2 | 4 => {
                let per = 8 / bpc as usize;
                let max = (1u16 << bpc) - 1;
                let mut n = 0usize;
                'row: for &byte in r {
                    for k in 0..per {
                        let shift = 8 - bpc as usize * (k + 1);
                        let v = ((byte as u16 >> shift) & max) * 255 / max;
                        samples.push(v as u8);
                        n += 1;
                        if n == width as usize * components {
                            break 'row;
                        }
                    }
                }
            }
            16 => {
                for pair in r.chunks_exact(2).take(width as usize * components) {
                    samples.push(pair[0]);
                }
            }
            _ => bail!("unsupported BitsPerComponent {bpc}"),
        }
    }

    // /Decode [1 0] inverts the sample ramp (commonly on 1-bit and
    // CCITT-sourced bitmaps); ImageMask stencils render as grayscale
    // (painted = black), which /Decode may flip.
    let inverted = match pdf.dict_get(dict, b"Decode")? {
        Some(Val::Array(a)) => {
            let v: Vec<f64> = a
                .iter()
                .filter_map(|o| pdf.resolve(o).ok().and_then(|v| v.as_num()))
                .collect();
            v.len() >= 2 && v[0] > v[1]
        }
        _ => false,
    };
    let is_mask = matches!(pdf.dict_get(dict, b"ImageMask")?, Some(Val::Bool(true)));
    if (inverted != is_mask) != tint_inverted {
        // ImageMask default paints 1-bits; without /Decode flip that
        // means sample 1 = ink = black, which the 0..255 ramp already
        // inverted once — XOR keeps both conventions straight.
        for s in samples.iter_mut() {
            *s = 255 - *s;
        }
    }

    // DeviceN with several colorants: collapse to the mean tint so the
    // grayscale path below applies.
    let (components, samples) = if tint_inverted && components > 1 {
        let mut gray = Vec::with_capacity(samples.len() / components);
        for px in samples.chunks_exact(components) {
            let sum: u32 = px.iter().map(|&v| v as u32).sum();
            gray.push((sum / components as u32) as u8);
        }
        (1usize, gray)
    } else {
        (components, samples)
    };

    // Expand palette / convert CMYK to what PNG can carry.
    let (color, final_samples) = match (&palette, components) {
        (Some(pal), 1) => {
            let mut rgb = Vec::with_capacity(samples.len() * 3);
            for &i in &samples {
                let o = i as usize * 3;
                if o + 2 < pal.len() {
                    rgb.extend_from_slice(&pal[o..o + 3]);
                } else {
                    rgb.extend_from_slice(&[0, 0, 0]);
                }
            }
            (png::ColorType::Rgb, rgb)
        }
        (None, 1) => (png::ColorType::Grayscale, samples),
        (None, 3) => (png::ColorType::Rgb, samples),
        (None, 4) => {
            // CMYK → RGB (naive but serviceable).
            let mut rgb = Vec::with_capacity(samples.len() / 4 * 3);
            for px in samples.chunks_exact(4) {
                let (c, m, y, k) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32);
                rgb.push((255 - (c + k).min(255)) as u8);
                rgb.push((255 - (m + k).min(255)) as u8);
                rgb.push((255 - (y + k).min(255)) as u8);
            }
            (png::ColorType::Rgb, rgb)
        }
        _ => bail!("unsupported component count {components}"),
    };

    // /SMask: attach the soft mask as a PNG alpha channel when its
    // dimensions match.
    let (color, final_samples) = match smask_alpha(pdf, dict, width, height)? {
        Some(alpha) => match color {
            png::ColorType::Grayscale => {
                let mut ga = Vec::with_capacity(final_samples.len() * 2);
                for (i, &g) in final_samples.iter().enumerate() {
                    ga.push(g);
                    ga.push(alpha[i]);
                }
                (png::ColorType::GrayscaleAlpha, ga)
            }
            png::ColorType::Rgb => {
                let mut rgba = Vec::with_capacity(final_samples.len() / 3 * 4);
                for (i, px) in final_samples.chunks_exact(3).enumerate() {
                    rgba.extend_from_slice(px);
                    rgba.push(alpha[i]);
                }
                (png::ColorType::Rgba, rgba)
            }
            _ => (color, final_samples),
        },
        None => (color, final_samples),
    };

    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(color);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| anyhow!("png: {e}"))?;
        w.write_image_data(&final_samples)
            .map_err(|e| anyhow!("png: {e}"))?;
    }
    Ok(ExtractedImage {
        bytes: out,
        ext: "png",
    })
}

/// Decode an image's /SMask stream into one alpha byte per pixel, when
/// present and dimension-matched.
fn smask_alpha(pdf: &Pdf, dict: &Dict, width: u32, height: u32) -> Result<Option<Vec<u8>>> {
    let Some(Val::Stream(sd, sraw)) = pdf.dict_get(dict, b"SMask")? else {
        return Ok(None);
    };
    let sw = pdf
        .dict_get(&sd, b"Width")?
        .and_then(|v| v.as_num())
        .unwrap_or(0.0) as u32;
    let sh = pdf
        .dict_get(&sd, b"Height")?
        .and_then(|v| v.as_num())
        .unwrap_or(0.0) as u32;
    if sw != width || sh != height {
        return Ok(None); // scaled masks: skip rather than resample
    }
    let sbpc = pdf
        .dict_get(&sd, b"BitsPerComponent")?
        .and_then(|v| v.as_num())
        .unwrap_or(8.0) as u32;
    // DCT-coded masks would need a JPEG decode; keep to bitmap masks.
    let filters = match pdf.dict_get(&sd, b"Filter")? {
        Some(Val::Name(n)) => vec![n.to_vec()],
        Some(Val::Array(a)) => a
            .iter()
            .filter_map(|v| pdf.resolve(v).ok())
            .filter_map(|v| v.as_name().map(|n| n.to_vec()))
            .collect(),
        _ => Vec::new(),
    };
    if filters
        .iter()
        .any(|f| matches!(f.as_slice(), b"DCTDecode" | b"JPXDecode" | b"JBIG2Decode"))
    {
        return Ok(None);
    }
    let data = decode_stream(&sd, sraw, pdf)?;
    let row_in = (width as usize * sbpc as usize).div_ceil(8);
    if data.len() < row_in * height as usize {
        return Ok(None);
    }
    let mut alpha = Vec::with_capacity(width as usize * height as usize);
    for row in 0..height as usize {
        let r = &data[row * row_in..(row + 1) * row_in];
        match sbpc {
            8 => alpha.extend_from_slice(&r[..width as usize]),
            16 => {
                for pair in r.chunks_exact(2).take(width as usize) {
                    alpha.push(pair[0]);
                }
            }
            1 | 2 | 4 => {
                let per = 8 / sbpc as usize;
                let max = (1u16 << sbpc) - 1;
                let mut n = 0usize;
                'row: for &byte in r {
                    for k in 0..per {
                        let shift = 8 - sbpc as usize * (k + 1);
                        let v = ((byte as u16 >> shift) & max) * 255 / max;
                        alpha.push(v as u8);
                        n += 1;
                        if n == width as usize {
                            break 'row;
                        }
                    }
                }
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(alpha))
}

/// (components per sample, optional RGB palette).
/// (components per sample, optional RGB palette, tint-inverted).
/// Separation/DeviceN tints run 0 = none to 1 = full colorant, i.e. the
/// opposite of a luminance ramp.
fn color_info(pdf: &Pdf, dict: &Dict) -> Result<(usize, Option<Vec<u8>>, bool)> {
    match pdf.dict_get(dict, b"ColorSpace")? {
        None | Some(Val::Name(b"DeviceGray")) | Some(Val::Name(b"CalGray")) => Ok((1, None, false)),
        Some(Val::Name(b"DeviceRGB")) | Some(Val::Name(b"CalRGB")) => Ok((3, None, false)),
        Some(Val::Name(b"DeviceCMYK")) => Ok((4, None, false)),
        Some(Val::Array(a)) => {
            let head = a.first().and_then(|v| v.as_name()).unwrap_or(b"");
            match head {
                b"ICCBased" => {
                    let n = match a.get(1).map(|v| pdf.resolve(v)) {
                        Some(Ok(Val::Stream(sd, _))) => {
                            pdf.dict_get(&sd, b"N")?
                                .and_then(|v| v.as_num())
                                .unwrap_or(3.0) as usize
                        }
                        _ => 3,
                    };
                    Ok((n, None, false))
                }
                b"Indexed" => {
                    // [/Indexed base hival lookup] — palette in base space
                    // (assume RGB triples; gray bases are rare for images).
                    let lookup = match a.get(3).map(|v| pdf.resolve(v)) {
                        Some(Ok(Val::Str(s))) => s,
                        Some(Ok(Val::Stream(sd, raw))) => decode_stream(&sd, raw, pdf)?,
                        _ => bail!("bad Indexed lookup"),
                    };
                    Ok((1, Some(lookup), false))
                }
                b"Separation" => Ok((1, None, true)),
                b"DeviceN" => {
                    let components = match a.get(1).map(|v| pdf.resolve(v)) {
                        Some(Ok(Val::Array(names))) => names.len().max(1),
                        _ => 1,
                    };
                    Ok((components, None, true))
                }
                _ => bail!("unsupported colorspace"),
            }
        }
        _ => bail!("unsupported colorspace"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf() -> Pdf<'static> {
        Pdf::parse(
            b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj
trailer << /Root 1 0 R >>",
        )
        .unwrap()
    }

    #[test]
    fn devicen_uses_colorant_count_and_tint_polarity() {
        let dict = vec![(
            b"ColorSpace".as_slice(),
            Val::Array(vec![
                Val::Name(b"DeviceN"),
                Val::Array(vec![Val::Name(b"Cyan"), Val::Name(b"Spot")]),
                Val::Name(b"DeviceCMYK"),
                Val::Null,
            ]),
        )];
        assert_eq!(color_info(&pdf(), &dict).unwrap(), (2, None, true));
    }

    #[test]
    fn separation_tint_is_inverted_for_luminance() {
        let dict = vec![(
            b"ColorSpace".as_slice(),
            Val::Array(vec![
                Val::Name(b"Separation"),
                Val::Name(b"Spot"),
                Val::Name(b"DeviceCMYK"),
                Val::Null,
            ]),
        )];
        assert_eq!(color_info(&pdf(), &dict).unwrap(), (1, None, true));
    }

    #[test]
    fn ccitt_columns_default_to_image_width() {
        let dict = vec![
            (b"Width".as_slice(), Val::Num(8.0)),
            (b"Height".as_slice(), Val::Num(2.0)),
            (
                b"DecodeParms".as_slice(),
                Val::Dict(vec![(b"K".as_slice(), Val::Num(-1.0))]),
            ),
        ];
        // Two all-white G4 rows (V0, V0).
        let image = extract_ccitt(&pdf(), &dict, &[0b1100_0000]).unwrap();
        let width = u32::from_be_bytes(image.bytes[16..20].try_into().unwrap());
        assert_eq!(width, 8);
    }

    #[test]
    fn jbig2_globals_follow_the_jbig2_filter_position() {
        let globals = Val::Stream(vec![(b"Length".as_slice(), Val::Num(3.0))], b"abc");
        let dict = vec![(
            b"DecodeParms".as_slice(),
            Val::Array(vec![
                Val::Dict(Vec::new()),
                Val::Dict(vec![(b"JBIG2Globals".as_slice(), globals)]),
            ]),
        )];
        assert_eq!(
            jbig2_globals(&pdf(), &dict, 1).as_deref(),
            Some(b"abc".as_slice())
        );
    }
}
