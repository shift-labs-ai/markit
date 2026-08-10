//! Embedded-image extraction from PDF pages — the pure-Rust replacement
//! for MuPDF region rasterization. Instead of rendering a page crop, the
//! placed image XObject itself is extracted at native resolution:
//! DCTDecode streams pass through as JPEG files, JPXDecode as JP2;
//! CCITT decodes to grayscale PNG; FlateDecode (and raw) bitmaps
//! re-encode as PNG; JBIG2 decodes through hayro-jbig2 (pure Rust).

use anyhow::{anyhow, bail, Result};

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
    let (dict, raw) = placements
        .into_iter()
        .nth(idx)
        .ok_or_else(|| anyhow!("image index out of range"))?;
    if raw.is_empty() {
        bail!("inline image: no extractable stream");
    }

    extract_xobject(&pdf, &dict, raw)
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
    let columns = pg(b"Columns", 1728.0) as usize;
    let cols = if columns > 0 { columns } else { width };
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
        // JBIG2Globals stream from DecodeParms, when present.
        let globals: Option<Vec<u8>> = (|| {
            let parms = match pdf.dict_get(dict, b"DecodeParms").ok()?? {
                Val::Dict(d) => Some(d),
                Val::Array(a) => a.iter().find_map(|v| match pdf.resolve(v) {
                    Ok(Val::Dict(d)) => Some(d),
                    _ => None,
                }),
                _ => None,
            }?;
            match pdf.dict_get(&parms, b"JBIG2Globals").ok()?? {
                Val::Stream(gd, graw) => decode_stream(&gd, graw, pdf).ok(),
                _ => None,
            }
        })();
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

    let (components, palette) = color_info(pdf, dict)?;
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

/// (components per sample, optional RGB palette).
fn color_info(pdf: &Pdf, dict: &Dict) -> Result<(usize, Option<Vec<u8>>)> {
    match pdf.dict_get(dict, b"ColorSpace")? {
        None | Some(Val::Name(b"DeviceGray")) | Some(Val::Name(b"CalGray")) => Ok((1, None)),
        Some(Val::Name(b"DeviceRGB")) | Some(Val::Name(b"CalRGB")) => Ok((3, None)),
        Some(Val::Name(b"DeviceCMYK")) => Ok((4, None)),
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
                    Ok((n, None))
                }
                b"Indexed" => {
                    // [/Indexed base hival lookup] — palette in base space
                    // (assume RGB triples; gray bases are rare for images).
                    let lookup = match a.get(3).map(|v| pdf.resolve(v)) {
                        Some(Ok(Val::Str(s))) => s,
                        Some(Ok(Val::Stream(sd, raw))) => decode_stream(&sd, raw, pdf)?,
                        _ => bail!("bad Indexed lookup"),
                    };
                    Ok((1, Some(lookup)))
                }
                b"DeviceN" | b"Separation" => Ok((1, None)),
                _ => bail!("unsupported colorspace"),
            }
        }
        _ => bail!("unsupported colorspace"),
    }
}
