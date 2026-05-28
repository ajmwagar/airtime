//! Segment sequencer.
//!
//! The mixer is the heart of the per-station task. It:
//!
//! 1. Picks the next slot (track / break) from the scheduler.
//! 2. Pre-renders the segment via the LLM + TTS + audio pipeline while
//!    the current segment is still streaming (queue depth ≥ 1).
//! 3. Encodes the rendered FLAC into the configured stream format and
//!    shovels chunks into the Icecast source channel.

use crate::audio::{ffmpeg_bin, AudioError};
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info};

/// What we tell Icecast we're sending, and what ffmpeg flags get us
/// there. Per-persona via `[stream]` in the persona TOML.
///
/// **MP3** — universal listener compatibility, lossy. Default for new
/// personas. `audio/mpeg`.
///
/// **Opus** — modern, efficient, very forgiving of chained-stream
/// boundaries. Lossy. `application/ogg`.
///
/// **OggFlac** — lossless, but: ~1 Mbps, narrow listener support
/// (no Safari iOS, spotty mobile), and FLAC decoders refuse mid-chain
/// format changes — so we normalise to a fixed 24-bit / 48 kHz / stereo
/// shape. `application/ogg`.
#[derive(Debug, Clone)]
pub enum StreamFormat {
    Mp3 { bitrate_kbps: u32 },
    Opus { bitrate_kbps: u32 },
    OggFlac,
}

impl StreamFormat {
    /// Parse the persona TOML `[stream] format = "…"` + optional
    /// `bitrate = N` (kbps). Format names are case-insensitive.
    pub fn parse(name: &str, bitrate_kbps: Option<u32>) -> Result<Self, String> {
        match name.to_ascii_lowercase().as_str() {
            "mp3" => Ok(StreamFormat::Mp3 {
                bitrate_kbps: bitrate_kbps.unwrap_or(256),
            }),
            "opus" => Ok(StreamFormat::Opus {
                bitrate_kbps: bitrate_kbps.unwrap_or(160),
            }),
            "flac" | "ogg-flac" | "ogg/flac" => Ok(StreamFormat::OggFlac),
            other => Err(format!(
                "unsupported stream format `{other}` — expected one of: mp3, opus, flac"
            )),
        }
    }

    /// Content-Type to advertise on the Icecast source request.
    pub fn content_type(&self) -> &'static str {
        match self {
            StreamFormat::Mp3 { .. } => "audio/mpeg",
            StreamFormat::Opus { .. } | StreamFormat::OggFlac => "application/ogg",
        }
    }

    /// ffmpeg output args — everything after `-i {input}`, ending in `-`
    /// for stdout. The intermediate format produced by `AudioProcessor`
    /// (FLAC) is the assumed input.
    pub fn ffmpeg_encode_args(&self) -> Vec<String> {
        match self {
            StreamFormat::Mp3 { bitrate_kbps } => vec![
                "-ar".into(),
                "44100".into(),
                "-ac".into(),
                "2".into(),
                "-c:a".into(),
                "libmp3lame".into(),
                "-b:a".into(),
                format!("{bitrate_kbps}k"),
                "-f".into(),
                "mp3".into(),
                "-".into(),
            ],
            StreamFormat::Opus { bitrate_kbps } => vec![
                "-ar".into(),
                "48000".into(),
                "-ac".into(),
                "2".into(),
                "-c:a".into(),
                "libopus".into(),
                "-b:a".into(),
                format!("{bitrate_kbps}k"),
                "-vbr".into(),
                "on".into(),
                "-f".into(),
                "ogg".into(),
                "-".into(),
            ],
            StreamFormat::OggFlac => vec![
                // Normalise to a fixed 24-bit / 48 kHz / stereo shape so
                // chained Ogg/FLAC streams have identical headers and
                // listener decoders don't trip on `switching bps
                // mid-stream is not supported`.
                "-ar".into(),
                "48000".into(),
                "-ac".into(),
                "2".into(),
                "-sample_fmt".into(),
                "s32".into(),
                "-bits_per_raw_sample".into(),
                "24".into(),
                "-c:a".into(),
                "flac".into(),
                "-f".into(),
                "ogg".into(),
                "-".into(),
            ],
        }
    }
}

/// Encode `src_path` to the configured `StreamFormat` and push the
/// bytes into `sink` in `chunk_size`-sized pieces. Returns when ffmpeg
/// exits.
///
/// **Real-time pacing:** the `-re` flag tells ffmpeg to read the input
/// at its native frame rate, so a 3-minute song takes 3 wall-clock
/// minutes to pump. Without it, ffmpeg encodes + writes as fast as the
/// CPU + pipe allow — bytes queue up in `sink`, the producer races
/// ahead, listeners hear delayed content, and the idle gaps between
/// back-to-back pumps trigger Icecast's `source-timeout`. With `-re`
/// the consumer naturally back-pressures the upstream channel.
pub async fn pump_to_icecast(
    src_path: &Path,
    sink: &mpsc::Sender<Vec<u8>>,
    chunk_size: usize,
    format: &StreamFormat,
) -> Result<(), AudioError> {
    let path_str = src_path.to_string_lossy().into_owned();
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-re".into(),
        "-i".into(),
        path_str.clone(),
    ];
    args.extend(format.ffmpeg_encode_args());

    let mut child = Command::new(ffmpeg_bin())
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AudioError::FfmpegMissing
            } else {
                AudioError::Io(e)
            }
        })?;
    let mut stdout = child.stdout.take().expect("ffmpeg stdout");
    // Drain stderr concurrently so ffmpeg can't block on a full stderr
    // pipe (default ~64 KiB).
    let stderr = child.stderr.take().expect("ffmpeg stderr");
    let stderr_handle: JoinHandle<Vec<u8>> = tokio::spawn(async move {
        let mut buf = Vec::with_capacity(4096);
        let mut reader = stderr;
        let _ = reader.read_to_end(&mut buf).await;
        buf
    });

    debug!(path = %path_str, ?format, chunk_size, "pump: ffmpeg started");

    let mut buf = vec![0u8; chunk_size.max(4096)];
    let mut total_bytes: u64 = 0;
    let mut chunks: u64 = 0;
    loop {
        let n = stdout.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if sink.send(buf[..n].to_vec()).await.is_err() {
            let _ = child.kill().await;
            break;
        }
        total_bytes += n as u64;
        chunks += 1;
        if chunks == 1 {
            debug!(path = %path_str, first_chunk_bytes = n, "pump: first bytes out");
        }
    }
    let status = child.wait().await?;
    let stderr_bytes = stderr_handle.await.unwrap_or_default();
    let stderr_text = String::from_utf8_lossy(&stderr_bytes).into_owned();
    info!(
        path = %path_str,
        bytes = total_bytes,
        chunks,
        exit_code = status.code().unwrap_or(-1),
        "pump: ffmpeg exited"
    );
    if !status.success() {
        return Err(AudioError::FfmpegFailed {
            code: status.code().unwrap_or(-1),
            stderr: if stderr_text.is_empty() {
                "ffmpeg pump failed (no stderr captured)".into()
            } else {
                stderr_text
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_known_formats_with_defaults() {
        let mp3 = StreamFormat::parse("mp3", None).unwrap();
        assert!(matches!(mp3, StreamFormat::Mp3 { bitrate_kbps: 256 }));
        let opus = StreamFormat::parse("opus", None).unwrap();
        assert!(matches!(opus, StreamFormat::Opus { bitrate_kbps: 160 }));
        let flac = StreamFormat::parse("FLAC", None).unwrap();
        assert!(matches!(flac, StreamFormat::OggFlac));
        let flac_alt = StreamFormat::parse("ogg-flac", None).unwrap();
        assert!(matches!(flac_alt, StreamFormat::OggFlac));
    }

    #[test]
    fn parse_honors_explicit_bitrate() {
        let mp3 = StreamFormat::parse("mp3", Some(192)).unwrap();
        assert!(matches!(mp3, StreamFormat::Mp3 { bitrate_kbps: 192 }));
        let opus = StreamFormat::parse("opus", Some(96)).unwrap();
        assert!(matches!(opus, StreamFormat::Opus { bitrate_kbps: 96 }));
    }

    #[test]
    fn parse_rejects_unknown_format() {
        let err = StreamFormat::parse("wav", None).unwrap_err();
        assert!(err.contains("unsupported stream format"));
        assert!(err.contains("mp3, opus, flac"));
    }

    #[test]
    fn content_type_matches_format() {
        assert_eq!(
            StreamFormat::Mp3 { bitrate_kbps: 256 }.content_type(),
            "audio/mpeg"
        );
        assert_eq!(
            StreamFormat::Opus { bitrate_kbps: 160 }.content_type(),
            "application/ogg"
        );
        assert_eq!(StreamFormat::OggFlac.content_type(), "application/ogg");
    }

    #[test]
    fn mp3_encode_args_include_libmp3lame_and_bitrate() {
        let args = StreamFormat::Mp3 { bitrate_kbps: 192 }.ffmpeg_encode_args();
        assert!(args.iter().any(|a| a == "libmp3lame"));
        assert!(args.iter().any(|a| a == "192k"));
        assert!(args.iter().any(|a| a == "mp3"));
        assert_eq!(args.last().map(|s| s.as_str()), Some("-"));
    }

    #[test]
    fn opus_encode_args_use_libopus_and_ogg_container() {
        let args = StreamFormat::Opus { bitrate_kbps: 96 }.ffmpeg_encode_args();
        assert!(args.iter().any(|a| a == "libopus"));
        assert!(args.iter().any(|a| a == "96k"));
        assert!(args.iter().any(|a| a == "ogg"));
    }

    #[test]
    fn flac_encode_args_normalise_format() {
        let args = StreamFormat::OggFlac.ffmpeg_encode_args();
        assert!(args.iter().any(|a| a == "48000"));
        assert!(args.iter().any(|a| a == "24"));
        assert!(args.iter().any(|a| a == "flac"));
        assert!(args.iter().any(|a| a == "ogg"));
    }

    /// Both subprocess error paths (binary missing, binary fails) are
    /// asserted in a single test so the shared `FFMPEG_BIN` env var
    /// doesn't race against itself when `cargo test` parallelizes.
    #[tokio::test]
    async fn surfaces_subprocess_errors() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("dummy.flac");
        std::fs::write(&p, b"not-really-flac").unwrap();
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(1);
        let fmt = StreamFormat::Mp3 { bitrate_kbps: 128 };

        std::env::set_var("FFMPEG_BIN", "/nope/nope/nope/ffmpeg");
        let err = pump_to_icecast(&p, &tx, 1024, &fmt).await.unwrap_err();
        assert!(matches!(err, AudioError::FfmpegMissing));

        std::env::set_var("FFMPEG_BIN", "/bin/false");
        let err = pump_to_icecast(&p, &tx, 1024, &fmt).await.unwrap_err();
        assert!(matches!(err, AudioError::FfmpegFailed { .. }));

        std::env::remove_var("FFMPEG_BIN");
    }
}
