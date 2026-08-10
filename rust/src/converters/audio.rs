use anyhow::Result;
use lofty::file::FileType;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::ItemKey;

use crate::types::{ConversionResult, Converter, StreamInfo};

const EXTENSIONS: &[&str] = &[
    ".mp3", ".wav", ".m4a", ".mp4", ".ogg", ".flac", ".aac", ".wma",
];

pub struct AudioConverter;

impl Converter for AudioConverter {
    fn name(&self) -> &'static str {
        "audio"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ext) = &info.extension {
            if EXTENSIONS.contains(&ext.as_str()) {
                return true;
            }
        }
        if let Some(mime) = &info.mimetype {
            if mime.starts_with("audio/") || mime == "video/mp4" {
                return true;
            }
        }
        false
    }

    fn convert(&self, input: &[u8], info: &StreamInfo) -> Result<ConversionResult> {
        let mut sections: Vec<String> = Vec::new();

        // Extract audio metadata via lofty
        if let Some(tagged_file) = Probe::new(std::io::Cursor::new(input))
            .guess_file_type()
            .ok()
            .and_then(|p| p.read().ok())
        {
            sections.push("## Metadata\n".to_string());

            let props = tagged_file.properties();
            let duration_secs = props.duration().as_secs_f64();
            let audio_bitrate = props.audio_bitrate(); // kbps
            let sample_rate = props.sample_rate();
            let channels = props.channels();

            let format_str = codec_string(tagged_file.file_type(), input);

            // Try to get tag from primary or first tag
            let tag_opt = tagged_file
                .primary_tag()
                .or_else(|| tagged_file.first_tag());

            let mut title: Option<String> = None;
            let mut artist: Option<String> = None;
            let mut album: Option<String> = None;
            let mut genre: Option<String> = None;
            let mut track_no: Option<u32> = None;
            let mut track_total: Option<u32> = None;
            let mut year: Option<u32> = None;
            let mut lyrics: Option<String> = None;

            if let Some(tag) = tag_opt {
                title = tag.title().map(|s| s.into_owned());
                artist = tag.artist().map(|s| s.into_owned());
                album = tag.album().map(|s| s.into_owned());
                genre = tag.genre().map(|s| s.into_owned());
                track_no = tag.track();
                track_total = tag.track_total();
                year = tag.date().map(|d| d.year as u32);
                lyrics = tag
                    .get_string(ItemKey::Lyrics)
                    .or_else(|| tag.get_string(ItemKey::UnsyncLyrics))
                    .map(str::to_string);
            }

            // Build fields in the same order as TS
            if let Some(v) = title {
                sections.push(format!("Title: {v}"));
            }
            if let Some(v) = artist {
                sections.push(format!("Artist: {v}"));
            }
            if let Some(v) = album {
                sections.push(format!("Album: {v}"));
            }
            if let Some(v) = genre {
                sections.push(format!("Genre: {v}"));
            }
            if let Some(no) = track_no {
                let s = if let Some(total) = track_total {
                    format!("{no} of {total}")
                } else {
                    format!("{no}")
                };
                sections.push(format!("Track: {s}"));
            }
            if let Some(y) = year {
                sections.push(format!("Year: {y}"));
            }
            if duration_secs > 0.0 {
                sections.push(format!("Duration: {}", format_duration(duration_secs)));
            }
            if let Some(v) = format_str {
                sections.push(format!("Format: {v}"));
            }
            if let Some(sr) = sample_rate {
                sections.push(format!("SampleRate: {sr} Hz"));
            }
            if let Some(ch) = channels {
                sections.push(format!("Channels: {ch}"));
            }
            if let Some(br) = audio_bitrate {
                sections.push(format!("Bitrate: {br} kbps"));
            }

            if let Some(lyr) = lyrics {
                if !lyr.is_empty() {
                    sections.push(format!("\n## Lyrics\n\n{lyr}"));
                }
            }
        }

        if sections.is_empty() {
            let name = info.filename.as_deref().unwrap_or("unknown");
            return Ok(ConversionResult::markdown(format!("*[audio: {name}]*")));
        }

        Ok(ConversionResult::markdown(
            sections.join("\n").trim().to_string(),
        ))
    }
}

/// Codec string matching music-metadata's `format.codec || format.container`
/// (the TS "Format:" line). Where lofty's generic properties lack codec
/// detail, the concrete file type is re-parsed.
fn codec_string(ft: FileType, input: &[u8]) -> Option<String> {
    use lofty::config::ParseOptions;
    match ft {
        FileType::Wav => {
            // music-metadata: WaveFormatNameMap[fmt.wFormatTag], e.g. "PCM".
            let file = lofty::iff::wav::WavFile::read_from(
                &mut std::io::Cursor::new(input),
                ParseOptions::new(),
            )
            .ok()?;
            Some(match file.properties().format() {
                lofty::iff::wav::WavFormat::PCM => "PCM".to_string(),
                lofty::iff::wav::WavFormat::IEEE_FLOAT => "IEEE_FLOAT".to_string(),
                lofty::iff::wav::WavFormat::Other(2) => "ADPCM".to_string(),
                lofty::iff::wav::WavFormat::Other(tag) => format!("non-PCM ({tag})"),
            })
        }
        FileType::Mpeg => {
            // music-metadata: `MPEG ${version} Layer ${layer}`.
            let file = lofty::mpeg::MpegFile::read_from(
                &mut std::io::Cursor::new(input),
                ParseOptions::new(),
            )
            .ok()?;
            let props = file.properties();
            let version = match props.version() {
                lofty::mpeg::MpegVersion::V1 => "1",
                lofty::mpeg::MpegVersion::V2 => "2",
                lofty::mpeg::MpegVersion::V2_5 => "2.5",
                _ => return Some("MPEG".to_string()),
            };
            let layer = match props.layer() {
                lofty::mpeg::Layer::Layer1 => 1,
                lofty::mpeg::Layer::Layer2 => 2,
                lofty::mpeg::Layer::Layer3 => 3,
            };
            Some(format!("MPEG {version} Layer {layer}"))
        }
        FileType::Mp4 => {
            // music-metadata: per-track encoder names, e.g. "MPEG-4/AAC", "ALAC".
            let file = lofty::mp4::Mp4File::read_from(
                &mut std::io::Cursor::new(input),
                ParseOptions::new(),
            )
            .ok()?;
            match file.properties().codec() {
                Some(lofty::mp4::Mp4Codec::AAC) => Some("MPEG-4/AAC".to_string()),
                Some(lofty::mp4::Mp4Codec::ALAC) => Some("ALAC".to_string()),
                Some(lofty::mp4::Mp4Codec::MP3) => Some("MPEG-1 layer 3".to_string()),
                Some(other) => Some(format!("{other:?}")),
                None => None,
            }
        }
        FileType::Flac => Some("FLAC".to_string()),
        FileType::Vorbis => Some("Vorbis I".to_string()),
        FileType::Opus => Some("Opus".to_string()),
        FileType::Aac => Some("AAC".to_string()),
        // music-metadata AIFF: compressionTypes.NONE for standard AIFF.
        FileType::Aiff => Some("not compressed\tPCM\tApple Computer".to_string()),
        // Rare formats music-metadata reports via container naming; keep
        // lofty's names (known minor divergence).
        FileType::Speex => Some("Speex".to_string()),
        FileType::Ape => Some("Monkey's Audio".to_string()),
        FileType::Mpc => Some("Musepack".to_string()),
        FileType::WavPack => Some("WavPack".to_string()),
        FileType::Custom(name) => Some(name.to_string()),
        _ => None,
    }
}

/// Format duration in seconds into "h:mm:ss" or "m:ss" matching the TS formatDuration.
fn format_duration(seconds: f64) -> String {
    let total = seconds.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_info(ext: Option<&str>, mime: Option<&str>, filename: Option<&str>) -> StreamInfo {
        StreamInfo {
            extension: ext.map(String::from),
            mimetype: mime.map(String::from),
            filename: filename.map(String::from),
            ..Default::default()
        }
    }

    // --- accepts() ---

    #[test]
    fn accepts_mp3_extension() {
        assert!(AudioConverter.accepts(&make_info(Some(".mp3"), None, None)));
    }

    #[test]
    fn accepts_wav_extension() {
        assert!(AudioConverter.accepts(&make_info(Some(".wav"), None, None)));
    }

    #[test]
    fn accepts_m4a_extension() {
        assert!(AudioConverter.accepts(&make_info(Some(".m4a"), None, None)));
    }

    #[test]
    fn accepts_mp4_extension() {
        assert!(AudioConverter.accepts(&make_info(Some(".mp4"), None, None)));
    }

    #[test]
    fn accepts_ogg_extension() {
        assert!(AudioConverter.accepts(&make_info(Some(".ogg"), None, None)));
    }

    #[test]
    fn accepts_flac_extension() {
        assert!(AudioConverter.accepts(&make_info(Some(".flac"), None, None)));
    }

    #[test]
    fn accepts_aac_extension() {
        assert!(AudioConverter.accepts(&make_info(Some(".aac"), None, None)));
    }

    #[test]
    fn accepts_wma_extension() {
        assert!(AudioConverter.accepts(&make_info(Some(".wma"), None, None)));
    }

    #[test]
    fn accepts_audio_mimetype_prefix() {
        assert!(AudioConverter.accepts(&make_info(None, Some("audio/mpeg"), None)));
        assert!(AudioConverter.accepts(&make_info(None, Some("audio/flac"), None)));
        assert!(AudioConverter.accepts(&make_info(None, Some("audio/wav"), None)));
        assert!(AudioConverter.accepts(&make_info(None, Some("audio/ogg"), None)));
    }

    #[test]
    fn accepts_video_mp4_mimetype() {
        // TS MIMETYPES includes "video/mp4"
        assert!(AudioConverter.accepts(&make_info(None, Some("video/mp4"), None)));
    }

    #[test]
    fn rejects_video_mp4_only_exact_string() {
        // "video/mp4" matches but "video/avi" should not
        assert!(!AudioConverter.accepts(&make_info(None, Some("video/avi"), None)));
        assert!(!AudioConverter.accepts(&make_info(None, Some("video/webm"), None)));
    }

    #[test]
    fn rejects_image_mimetype() {
        assert!(!AudioConverter.accepts(&make_info(None, Some("image/jpeg"), None)));
    }

    #[test]
    fn rejects_pdf_extension() {
        assert!(!AudioConverter.accepts(&make_info(Some(".pdf"), None, None)));
    }

    // --- placeholder ---

    #[test]
    fn placeholder_when_lofty_fails_with_filename() {
        let info = make_info(Some(".mp3"), None, Some("song.mp3"));
        let result = AudioConverter.convert(&[], &info).unwrap();
        assert_eq!(result.markdown, "*[audio: song.mp3]*");
    }

    #[test]
    fn placeholder_uses_unknown_when_no_filename() {
        let info = make_info(Some(".mp3"), None, None);
        let result = AudioConverter.convert(&[], &info).unwrap();
        assert_eq!(result.markdown, "*[audio: unknown]*");
    }

    // --- WAV fixture ---

    /// Build a minimal valid PCM WAV with a few audio samples.
    /// lofty requires a non-empty data chunk (stream_len > 0) to parse properties.
    fn minimal_wav() -> Vec<u8> {
        // 10 samples of silence: 8-bit unsigned PCM, 1 channel, 44100 Hz
        let samples: Vec<u8> = vec![0x80u8; 10]; // 10 bytes of silence
        let data_len = samples.len() as u32;
        let chunk_size = 36u32 + data_len; // "WAVE" + fmt(24) + data header(8) + data

        let mut wav: Vec<u8> = Vec::new();
        // RIFF header
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&chunk_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        // fmt sub-chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // SubChunk1Size
        wav.extend_from_slice(&1u16.to_le_bytes()); // AudioFormat = PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // NumChannels = 1
        wav.extend_from_slice(&44100u32.to_le_bytes()); // SampleRate
        wav.extend_from_slice(&44100u32.to_le_bytes()); // ByteRate = SR * 1 * 1
        wav.extend_from_slice(&1u16.to_le_bytes()); // BlockAlign
        wav.extend_from_slice(&8u16.to_le_bytes()); // BitsPerSample
                                                    // data sub-chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(&samples);
        wav
    }

    #[test]
    fn wav_produces_metadata_section() {
        let wav = minimal_wav();
        let info = make_info(Some(".wav"), Some("audio/wav"), Some("test.wav"));
        let result = AudioConverter.convert(&wav, &info).unwrap();
        // Should have a Metadata section (even if fields are mostly empty)
        assert!(
            result.markdown.contains("## Metadata"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn wav_reports_sample_rate() {
        let wav = minimal_wav();
        let info = make_info(Some(".wav"), Some("audio/wav"), Some("test.wav"));
        let result = AudioConverter.convert(&wav, &info).unwrap();
        assert!(
            result.markdown.contains("44100 Hz"),
            "got: {}",
            result.markdown
        );
    }

    #[test]
    fn wav_reports_channel_count() {
        let wav = minimal_wav();
        let info = make_info(Some(".wav"), Some("audio/wav"), Some("test.wav"));
        let result = AudioConverter.convert(&wav, &info).unwrap();
        assert!(
            result.markdown.contains("Channels: 1"),
            "got: {}",
            result.markdown
        );
    }

    // --- format_duration ---

    #[test]
    fn duration_under_one_hour() {
        assert_eq!(format_duration(125.0), "2:05");
        assert_eq!(format_duration(60.0), "1:00");
        assert_eq!(format_duration(0.0), "0:00");
    }

    #[test]
    fn duration_over_one_hour() {
        assert_eq!(format_duration(3661.0), "1:01:01");
        assert_eq!(format_duration(7200.0), "2:00:00");
    }
}
