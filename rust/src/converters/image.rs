use anyhow::Result;
use std::io::Cursor;

use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

const EXTENSIONS: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".gif", ".webp", ".tiff", ".tif", ".bmp", ".svg",
];

pub struct ImageConverter;

impl Converter for ImageConverter {
    fn name(&self) -> &'static str {
        "image"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ext) = &info.extension {
            if EXTENSIONS.contains(&ext.as_str()) {
                return true;
            }
        }
        if let Some(mime) = &info.mimetype {
            if mime.starts_with("image/") {
                return true;
            }
        }
        false
    }

    fn convert(
        &self,
        input: &[u8],
        info: &StreamInfo,
        options: &MarkitOptions,
    ) -> Result<ConversionResult> {
        let mut sections: Vec<String> = Vec::new();

        // Extract EXIF metadata — gracefully skip if format not supported
        if let Ok(exif_lines) = parse_exif(input) {
            if !exif_lines.is_empty() {
                sections.push("## Metadata\n".to_string());
                for line in exif_lines {
                    sections.push(line);
                }
            }
        }

        // AI description hook
        if let Some(describe) = &options.describe {
            let mimetype = info
                .mimetype
                .clone()
                .unwrap_or_else(|| guess_mimetype(info.extension.as_deref()));
            if let Ok(description) = describe(input, &mimetype) {
                if !description.is_empty() {
                    sections.push(format!("\n## Description\n\n{description}"));
                }
            }
        }

        if sections.is_empty() {
            let name = info.filename.as_deref().unwrap_or("unknown");
            return Ok(ConversionResult::markdown(format!("*[image: {name}]*")));
        }

        Ok(ConversionResult::markdown(sections.join("\n").trim().to_string()))
    }
}

/// Parse EXIF and return ordered field lines matching the TS output format.
fn parse_exif(input: &[u8]) -> Result<Vec<String>> {
    use exif::{In, Reader, Tag, Value};

    let mut cursor = Cursor::new(input);
    let exif = Reader::new().read_from_container(&mut cursor)?;

    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let mut title: Option<String> = None;
    let mut description: Option<String> = None;
    let mut keywords: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut copyright: Option<String> = None;
    let mut make: Option<String> = None;
    let mut model: Option<String> = None;
    let mut datetime_original: Option<String> = None;
    let mut create_date: Option<String> = None;
    let mut gps_lat: Option<f64> = None;
    let mut gps_lat_ref: Option<String> = None;
    let mut gps_lon: Option<f64> = None;
    let mut gps_lon_ref: Option<String> = None;
    let mut exposure_time: Option<(u32, u32)> = None;
    let mut fnumber: Option<f64> = None;
    let mut iso: Option<u32> = None;
    let mut focal_length: Option<f64> = None;
    let mut software: Option<String> = None;

    // We need In to specify IFD, but iterate fields() which covers all IFDs
    let _ = In::PRIMARY; // ensure imported

    for field in exif.fields() {
        match field.tag {
            Tag::ImageWidth | Tag::PixelXDimension => {
                if width.is_none() {
                    width = field.value.get_uint(0);
                }
            }
            Tag::ImageLength | Tag::PixelYDimension => {
                if height.is_none() {
                    height = field.value.get_uint(0);
                }
            }
            Tag::ImageDescription => {
                if description.is_none() {
                    description = ascii_value(&field.value);
                }
            }
            Tag::Make => {
                if make.is_none() {
                    make = ascii_value(&field.value);
                }
            }
            Tag::Model => {
                if model.is_none() {
                    model = ascii_value(&field.value);
                }
            }
            Tag::Software => {
                if software.is_none() {
                    software = ascii_value(&field.value);
                }
            }
            Tag::Artist => {
                if artist.is_none() {
                    artist = ascii_value(&field.value);
                }
            }
            Tag::Copyright => {
                if copyright.is_none() {
                    copyright = ascii_value(&field.value);
                }
            }
            Tag::DateTimeOriginal => {
                if datetime_original.is_none() {
                    datetime_original = ascii_value(&field.value);
                }
            }
            Tag::DateTimeDigitized => {
                // exifr maps this as "CreateDate"
                if create_date.is_none() {
                    create_date = ascii_value(&field.value);
                }
            }
            Tag::GPSLatitude => {
                if gps_lat.is_none() {
                    gps_lat = dms_rational(&field.value);
                }
            }
            Tag::GPSLatitudeRef => {
                if gps_lat_ref.is_none() {
                    gps_lat_ref = ascii_value(&field.value).map(|s| s.trim().to_string());
                }
            }
            Tag::GPSLongitude => {
                if gps_lon.is_none() {
                    gps_lon = dms_rational(&field.value);
                }
            }
            Tag::GPSLongitudeRef => {
                if gps_lon_ref.is_none() {
                    gps_lon_ref = ascii_value(&field.value).map(|s| s.trim().to_string());
                }
            }
            Tag::ExposureTime => {
                if exposure_time.is_none() {
                    if let Value::Rational(ref v) = field.value {
                        if let Some(r) = v.first() {
                            exposure_time = Some((r.num, r.denom));
                        }
                    }
                }
            }
            Tag::FNumber => {
                if fnumber.is_none() {
                    if let Value::Rational(ref v) = field.value {
                        if let Some(r) = v.first() {
                            fnumber = Some(r.to_f64());
                        }
                    }
                }
            }
            Tag::PhotographicSensitivity => {
                if iso.is_none() {
                    iso = field.value.get_uint(0);
                }
            }
            Tag::FocalLength => {
                if focal_length.is_none() {
                    if let Value::Rational(ref v) = field.value {
                        if let Some(r) = v.first() {
                            focal_length = Some(r.to_f64());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Build field lines in the same order as the TS code
    let mut lines: Vec<String> = Vec::new();

    if let (Some(w), Some(h)) = (width, height) {
        lines.push(format!("ImageSize: {w}x{h}"));
    }
    if let Some(v) = title {
        lines.push(format!("Title: {v}"));
    }
    if let Some(v) = description {
        lines.push(format!("Description: {v}"));
    }
    if let Some(v) = keywords {
        lines.push(format!("Keywords: {v}"));
    }
    if let Some(v) = artist {
        lines.push(format!("Artist: {v}"));
    }
    if let Some(v) = copyright {
        lines.push(format!("Copyright: {v}"));
    }
    {
        let parts: Vec<&str> = [make.as_deref(), model.as_deref()]
            .iter()
            .filter_map(|s| *s)
            .collect();
        if !parts.is_empty() {
            lines.push(format!("Camera: {}", parts.join(" ")));
        }
    }
    if let Some(v) = datetime_original {
        lines.push(format!("DateTimeOriginal: {v}"));
    }
    if let Some(v) = create_date {
        lines.push(format!("CreateDate: {v}"));
    }
    if let (Some(lat), Some(lon)) = (gps_lat, gps_lon) {
        let lat_sign = if gps_lat_ref.as_deref() == Some("S") { -1.0_f64 } else { 1.0_f64 };
        let lon_sign = if gps_lon_ref.as_deref() == Some("W") { -1.0_f64 } else { 1.0_f64 };
        let lat_dec = lat * lat_sign;
        let lon_dec = lon * lon_sign;
        lines.push(format!("GPS: {lat_dec}, {lon_dec}"));
    }
    if let Some((num, denom)) = exposure_time {
        if num > 0 && denom > 0 {
            let val = num as f64 / denom as f64;
            let inv = (1.0 / val).round() as u64;
            lines.push(format!("ExposureTime: 1/{inv}s"));
        }
    }
    if let Some(f) = fnumber {
        lines.push(format!("FNumber: f/{}", format_float(f)));
    }
    if let Some(v) = iso {
        lines.push(format!("ISO: {v}"));
    }
    if let Some(fl) = focal_length {
        lines.push(format!("FocalLength: {}mm", format_float(fl)));
    }
    if let Some(v) = software {
        lines.push(format!("Software: {v}"));
    }

    Ok(lines)
}

fn ascii_value(value: &exif::Value) -> Option<String> {
    if let exif::Value::Ascii(ref v) = value {
        if let Some(bytes) = v.first() {
            let s = String::from_utf8_lossy(bytes);
            let trimmed = s.trim_end_matches('\0').trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

fn dms_rational(value: &exif::Value) -> Option<f64> {
    if let exif::Value::Rational(ref v) = value {
        if v.len() >= 3 {
            let deg = v[0].to_f64();
            let min = v[1].to_f64();
            let sec = v[2].to_f64();
            return Some(deg + min / 60.0 + sec / 3600.0);
        }
    }
    None
}

fn format_float(f: f64) -> String {
    if f == f.floor() && f >= 0.0 {
        format!("{}", f as i64)
    } else {
        let s = format!("{:.2}", f);
        let s = s.trim_end_matches('0');
        let s = s.trim_end_matches('.');
        s.to_string()
    }
}

fn guess_mimetype(ext: Option<&str>) -> String {
    match ext.unwrap_or("") {
        ".jpg" | ".jpeg" => "image/jpeg",
        ".png" => "image/png",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".tiff" | ".tif" => "image/tiff",
        ".bmp" => "image/bmp",
        ".svg" => "image/svg+xml",
        _ => "image/png",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MarkitOptions;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn make_info(ext: Option<&str>, mime: Option<&str>, filename: Option<&str>) -> StreamInfo {
        StreamInfo {
            extension: ext.map(String::from),
            mimetype: mime.map(String::from),
            filename: filename.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn accepts_jpg_extension() {
        assert!(ImageConverter.accepts(&make_info(Some(".jpg"), None, None)));
    }

    #[test]
    fn accepts_jpeg_extension() {
        assert!(ImageConverter.accepts(&make_info(Some(".jpeg"), None, None)));
    }

    #[test]
    fn accepts_png_extension() {
        assert!(ImageConverter.accepts(&make_info(Some(".png"), None, None)));
    }

    #[test]
    fn accepts_gif_extension() {
        assert!(ImageConverter.accepts(&make_info(Some(".gif"), None, None)));
    }

    #[test]
    fn accepts_webp_extension() {
        assert!(ImageConverter.accepts(&make_info(Some(".webp"), None, None)));
    }

    #[test]
    fn accepts_tiff_extension() {
        assert!(ImageConverter.accepts(&make_info(Some(".tiff"), None, None)));
    }

    #[test]
    fn accepts_tif_extension() {
        assert!(ImageConverter.accepts(&make_info(Some(".tif"), None, None)));
    }

    #[test]
    fn accepts_bmp_extension() {
        assert!(ImageConverter.accepts(&make_info(Some(".bmp"), None, None)));
    }

    #[test]
    fn accepts_svg_extension() {
        // SVG must be accepted — it is in the TS EXTENSIONS list
        assert!(ImageConverter.accepts(&make_info(Some(".svg"), None, None)));
    }

    #[test]
    fn accepts_image_mimetype_prefix() {
        assert!(ImageConverter.accepts(&make_info(None, Some("image/jpeg"), None)));
        assert!(ImageConverter.accepts(&make_info(None, Some("image/png"), None)));
        assert!(ImageConverter.accepts(&make_info(None, Some("image/svg+xml"), None)));
        assert!(ImageConverter.accepts(&make_info(None, Some("image/gif"), None)));
    }

    #[test]
    fn rejects_non_image_extension() {
        assert!(!ImageConverter.accepts(&make_info(Some(".pdf"), None, None)));
        assert!(!ImageConverter.accepts(&make_info(Some(".mp3"), None, None)));
        assert!(!ImageConverter.accepts(&make_info(Some(".mp4"), None, None)));
    }

    #[test]
    fn rejects_non_image_mimetype() {
        assert!(!ImageConverter.accepts(&make_info(None, Some("audio/mpeg"), None)));
        assert!(!ImageConverter.accepts(&make_info(None, Some("application/pdf"), None)));
        assert!(!ImageConverter.accepts(&make_info(None, Some("video/mp4"), None)));
    }

    #[test]
    fn placeholder_when_no_exif_and_no_describe() {
        let info = make_info(Some(".jpg"), None, Some("photo.jpg"));
        let result = ImageConverter.convert(&[], &info, &MarkitOptions::default()).unwrap();
        assert_eq!(result.markdown, "*[image: photo.jpg]*");
    }

    #[test]
    fn placeholder_uses_unknown_when_no_filename() {
        let info = make_info(Some(".png"), None, None);
        let result = ImageConverter.convert(&[], &info, &MarkitOptions::default()).unwrap();
        assert_eq!(result.markdown, "*[image: unknown]*");
    }

    #[test]
    fn svg_placeholder() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>";
        let info = make_info(Some(".svg"), Some("image/svg+xml"), Some("t.svg"));
        let result = ImageConverter.convert(svg, &info, &MarkitOptions::default()).unwrap();
        assert_eq!(result.markdown, "*[image: t.svg]*");
    }

    #[test]
    fn describe_hook_called_with_correct_mimetype() {
        let called = Arc::new(AtomicBool::new(false));
        let called2 = Arc::clone(&called);
        let opts = MarkitOptions {
            describe: Some(Box::new(move |_bytes, mime| {
                called2.store(true, Ordering::SeqCst);
                assert_eq!(mime, "image/jpeg");
                Ok("A red square.".to_string())
            })),
            ..Default::default()
        };
        let info = make_info(Some(".jpg"), None, Some("img.jpg"));
        let result = ImageConverter.convert(&[], &info, &opts).unwrap();
        assert!(called.load(Ordering::SeqCst));
        assert!(result.markdown.contains("## Description"));
        assert!(result.markdown.contains("A red square."));
    }

    #[test]
    fn describe_uses_streaminfo_mimetype_when_available() {
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let captured2 = Arc::clone(&captured);
        let opts = MarkitOptions {
            describe: Some(Box::new(move |_bytes, mime| {
                *captured2.lock().unwrap() = mime.to_string();
                Ok("desc".to_string())
            })),
            ..Default::default()
        };
        let info = make_info(None, Some("image/webp"), Some("img.webp"));
        ImageConverter.convert(&[], &info, &opts).unwrap();
        assert_eq!(*captured.lock().unwrap(), "image/webp");
    }

    #[test]
    fn describe_guesses_mimetype_from_extension() {
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let captured2 = Arc::clone(&captured);
        let opts = MarkitOptions {
            describe: Some(Box::new(move |_bytes, mime| {
                *captured2.lock().unwrap() = mime.to_string();
                Ok("desc".to_string())
            })),
            ..Default::default()
        };
        let info = make_info(Some(".png"), None, Some("img.png"));
        ImageConverter.convert(&[], &info, &opts).unwrap();
        assert_eq!(*captured.lock().unwrap(), "image/png");
    }

    #[test]
    fn describe_svg_guesses_svg_xml_mimetype() {
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let captured2 = Arc::clone(&captured);
        let opts = MarkitOptions {
            describe: Some(Box::new(move |_bytes, mime| {
                *captured2.lock().unwrap() = mime.to_string();
                Ok("desc".to_string())
            })),
            ..Default::default()
        };
        let info = make_info(Some(".svg"), None, Some("icon.svg"));
        ImageConverter.convert(&[], &info, &opts).unwrap();
        assert_eq!(*captured.lock().unwrap(), "image/svg+xml");
    }

    #[test]
    fn describe_failure_degrades_to_placeholder() {
        let opts = MarkitOptions {
            describe: Some(Box::new(|_bytes, _mime| Err(anyhow::anyhow!("LLM down")))),
            ..Default::default()
        };
        let info = make_info(Some(".jpg"), None, Some("img.jpg"));
        let result = ImageConverter.convert(&[], &info, &opts).unwrap();
        assert_eq!(result.markdown, "*[image: img.jpg]*");
    }

    /// Build a minimal JPEG with an EXIF block containing Make + Model.
    fn minimal_jpeg_exif(make: &str, model: &str) -> Vec<u8> {
        let make_bytes: Vec<u8> = make.bytes().chain(std::iter::once(0)).collect();
        let model_bytes: Vec<u8> = model.bytes().chain(std::iter::once(0)).collect();

        // TIFF header (LE) = 8 bytes
        // IFD0 at offset 8: count(2) + 2*entry(12) + next(4) = 30 bytes
        // Strings start at offset 8 + 30 = 38
        let make_offset: u32 = 38;
        let model_offset: u32 = 38 + make_bytes.len() as u32;

        let mut tiff: Vec<u8> = Vec::new();
        tiff.extend_from_slice(b"II"); // little-endian
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8

        // IFD0: 2 entries
        tiff.extend_from_slice(&2u16.to_le_bytes());

        // Tag: Make (0x010F), type ASCII (2)
        tiff.extend_from_slice(&0x010Fu16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&(make_bytes.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&make_offset.to_le_bytes());

        // Tag: Model (0x0110), type ASCII (2)
        tiff.extend_from_slice(&0x0110u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&(model_bytes.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&model_offset.to_le_bytes());

        // Next IFD offset
        tiff.extend_from_slice(&0u32.to_le_bytes());

        tiff.extend_from_slice(&make_bytes);
        tiff.extend_from_slice(&model_bytes);

        // Wrap in JPEG APP1
        let mut exif_block: Vec<u8> = b"Exif\0\0".to_vec();
        exif_block.extend_from_slice(&tiff);
        let app1_len = (exif_block.len() as u16 + 2).to_be_bytes();

        let mut jpeg: Vec<u8> = Vec::new();
        jpeg.extend_from_slice(b"\xFF\xD8");
        jpeg.extend_from_slice(b"\xFF\xE1");
        jpeg.extend_from_slice(&app1_len);
        jpeg.extend_from_slice(&exif_block);
        jpeg.extend_from_slice(b"\xFF\xD9");
        jpeg
    }

    #[test]
    fn exif_extracts_make_and_model_as_camera() {
        let jpeg = minimal_jpeg_exif("Canon", "EOS 5D");
        let info = make_info(Some(".jpg"), None, Some("photo.jpg"));
        let result = ImageConverter.convert(&jpeg, &info, &MarkitOptions::default()).unwrap();
        assert!(result.markdown.contains("## Metadata"), "missing header: {}", result.markdown);
        assert!(result.markdown.contains("Camera: Canon EOS 5D"), "missing camera: {}", result.markdown);
    }

    #[test]
    fn format_float_integer_no_decimal() {
        assert_eq!(format_float(50.0), "50");
        assert_eq!(format_float(2.0), "2");
        assert_eq!(format_float(1.0), "1");
    }

    #[test]
    fn format_float_decimal() {
        assert_eq!(format_float(2.8), "2.8");
        assert_eq!(format_float(5.6), "5.6");
        assert_eq!(format_float(1.4), "1.4");
    }

    #[test]
    fn guess_mimetype_returns_svg_xml() {
        assert_eq!(guess_mimetype(Some(".svg")), "image/svg+xml");
    }

    #[test]
    fn guess_mimetype_jpg_jpeg() {
        assert_eq!(guess_mimetype(Some(".jpg")), "image/jpeg");
        assert_eq!(guess_mimetype(Some(".jpeg")), "image/jpeg");
    }

    #[test]
    fn guess_mimetype_unknown_falls_back_to_png() {
        assert_eq!(guess_mimetype(Some(".xyz")), "image/png");
        assert_eq!(guess_mimetype(None), "image/png");
    }
}
