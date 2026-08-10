use anyhow::Result;
use lofty::file::FileType;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::ItemKey;

use crate::types::{ConversionResult, Converter, MarkitOptions, StreamInfo};

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

    fn convert(
        &self,
        input: &[u8],
        info: &StreamInfo,
        options: &MarkitOptions,
    ) -> Result<ConversionResult> {
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

            let format_str = file_type_to_format(tagged_file.file_type());

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

        // AI transcription hook
        if let Some(transcribe) = &options.transcribe {
            let mimetype = info
                .mimetype
                .clone()
                .unwrap_or_else(|| guess_mimetype(info.extension.as_deref()));
            if let Ok(transcript) = transcribe(input, &mimetype) {
                if !transcript.is_empty() {
                    sections.push(format!("\n## Transcript\n\n{transcript}"));
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

/// Convert lofty FileType to a format string, analogous to TS's format.codec || format.container.
fn file_type_to_format(ft: FileType) -> Option<String> {
    let s = match ft {
        FileType::Mpeg => "MPEG",
        FileType::Mp4 => "MP4",
        FileType::Flac => "FLAC",
        FileType::Wav => "WAV",
        FileType::Aiff => "AIFF",
        FileType::Aac => "AAC",
        FileType::Vorbis => "Vorbis",
        FileType::Opus => "Opus",
        FileType::Speex => "Speex",
        FileType::Ape => "Monkey's Audio",
        FileType::Mpc => "Musepack",
        FileType::WavPack => "WavPack",
        FileType::Custom(name) => name,
        _ => return None,
    };
    Some(s.to_string())
}

/// Format duration in seconds into "h:mm:ss" or "m:ss" matching the TS formatDuration.
pub fn format_duration(seconds: f64) -> String {
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

fn guess_mimetype(ext: Option<&str>) -> String {
    match ext.unwrap_or("") {
        ".mp3" => "audio/mpeg",
        ".wav" => "audio/wav",
        ".m4a" => "audio/mp4",
        ".mp4" => "video/mp4",
        ".ogg" => "audio/ogg",
        ".flac" => "audio/flac",
        ".aac" => "audio/aac",
        ".wma" => "audio/x-ms-wma",
        _ => "audio/mpeg",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MarkitOptions;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

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
        let result = AudioConverter
            .convert(&[], &info, &MarkitOptions::default())
            .unwrap();
        assert_eq!(result.markdown, "*[audio: song.mp3]*");
    }

    #[test]
    fn placeholder_uses_unknown_when_no_filename() {
        let info = make_info(Some(".mp3"), None, None);
        let result = AudioConverter
            .convert(&[], &info, &MarkitOptions::default())
            .unwrap();
        assert_eq!(result.markdown, "*[audio: unknown]*");
    }

    // --- transcribe hook ---

    #[test]
    fn transcribe_hook_called_with_correct_mimetype() {
        let called = Arc::new(AtomicBool::new(false));
        let called2 = Arc::clone(&called);
        let opts = MarkitOptions {
            transcribe: Some(Box::new(move |_bytes, mime| {
                called2.store(true, Ordering::SeqCst);
                assert_eq!(mime, "audio/mpeg");
                Ok("Hello world.".to_string())
            })),
            ..Default::default()
        };
        let info = make_info(Some(".mp3"), None, Some("song.mp3"));
        let result = AudioConverter.convert(&[], &info, &opts).unwrap();
        assert!(called.load(Ordering::SeqCst));
        assert!(result.markdown.contains("## Transcript"));
        assert!(result.markdown.contains("Hello world."));
    }

    #[test]
    fn transcribe_uses_streaminfo_mimetype() {
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let captured2 = Arc::clone(&captured);
        let opts = MarkitOptions {
            transcribe: Some(Box::new(move |_bytes, mime| {
                *captured2.lock().unwrap() = mime.to_string();
                Ok("transcript".to_string())
            })),
            ..Default::default()
        };
        let info = make_info(None, Some("audio/flac"), Some("song.flac"));
        AudioConverter.convert(&[], &info, &opts).unwrap();
        assert_eq!(*captured.lock().unwrap(), "audio/flac");
    }

    #[test]
    fn transcribe_guesses_mimetype_from_extension() {
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let captured2 = Arc::clone(&captured);
        let opts = MarkitOptions {
            transcribe: Some(Box::new(move |_bytes, mime| {
                *captured2.lock().unwrap() = mime.to_string();
                Ok("transcript".to_string())
            })),
            ..Default::default()
        };
        let info = make_info(Some(".wav"), None, None);
        AudioConverter.convert(&[], &info, &opts).unwrap();
        assert_eq!(*captured.lock().unwrap(), "audio/wav");
    }

    #[test]
    fn transcribe_failure_degrades_to_placeholder() {
        let opts = MarkitOptions {
            transcribe: Some(Box::new(|_bytes, _mime| Err(anyhow::anyhow!("STT failed")))),
            ..Default::default()
        };
        let info = make_info(Some(".mp3"), None, Some("song.mp3"));
        let result = AudioConverter.convert(&[], &info, &opts).unwrap();
        assert_eq!(result.markdown, "*[audio: song.mp3]*");
    }

    #[test]
    fn transcribe_appends_after_metadata_section() {
        let opts = MarkitOptions {
            transcribe: Some(Box::new(
                |_bytes, _mime| Ok("Transcribed text.".to_string()),
            )),
            ..Default::default()
        };
        let info = make_info(Some(".mp3"), None, Some("s.mp3"));
        let result = AudioConverter.convert(&[], &info, &opts).unwrap();
        let md = &result.markdown;
        // transcribe is called even when lofty fails; transcript appears in output
        assert!(md.contains("## Transcript"), "got: {md}");
        assert!(md.contains("Transcribed text."), "got: {md}");
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
        let result = AudioConverter
            .convert(&wav, &info, &MarkitOptions::default())
            .unwrap();
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
        let result = AudioConverter
            .convert(&wav, &info, &MarkitOptions::default())
            .unwrap();
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
        let result = AudioConverter
            .convert(&wav, &info, &MarkitOptions::default())
            .unwrap();
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

    #[test]
    fn duration_rounds_to_nearest_second() {
        assert_eq!(format_duration(125.4), "2:05");
        assert_eq!(format_duration(125.6), "2:06");
    }

    // --- guess_mimetype ---

    #[test]
    fn guess_mimetype_mp3() {
        assert_eq!(guess_mimetype(Some(".mp3")), "audio/mpeg");
    }

    #[test]
    fn guess_mimetype_wav() {
        assert_eq!(guess_mimetype(Some(".wav")), "audio/wav");
    }

    #[test]
    fn guess_mimetype_m4a() {
        assert_eq!(guess_mimetype(Some(".m4a")), "audio/mp4");
    }

    #[test]
    fn guess_mimetype_mp4() {
        assert_eq!(guess_mimetype(Some(".mp4")), "video/mp4");
    }

    #[test]
    fn guess_mimetype_ogg() {
        assert_eq!(guess_mimetype(Some(".ogg")), "audio/ogg");
    }

    #[test]
    fn guess_mimetype_flac() {
        assert_eq!(guess_mimetype(Some(".flac")), "audio/flac");
    }

    #[test]
    fn guess_mimetype_aac() {
        assert_eq!(guess_mimetype(Some(".aac")), "audio/aac");
    }

    #[test]
    fn guess_mimetype_wma() {
        assert_eq!(guess_mimetype(Some(".wma")), "audio/x-ms-wma");
    }

    #[test]
    fn guess_mimetype_unknown_fallback() {
        assert_eq!(guess_mimetype(Some(".xyz")), "audio/mpeg");
        assert_eq!(guess_mimetype(None), "audio/mpeg");
    }
}
