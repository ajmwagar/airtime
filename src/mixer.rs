//! Segment sequencer.
//!
//! The mixer is the heart of the per-station task. It:
//!
//! 1. Picks the next slot (track / break) from the scheduler.
//! 2. Pre-renders the segment via the LLM + TTS + audio pipeline while
//!    the current segment is still streaming (queue depth ≥ 1).
//! 3. Encodes the rendered FLAC into Ogg-framed FLAC and shovels chunks
//!    into the Icecast source channel.
//!
//! Encoding goes through FFmpeg: `ffmpeg -i file.flac -c:a copy -f ogg -`.
//! `-c:a copy` keeps it lossless — we're just re-muxing the FLAC
//! bitstream into an Ogg container so Icecast can fan it out.

use crate::audio::{ffmpeg_bin, AudioError};
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info};

/// Encode `flac_path` to Ogg/FLAC and push the bytes into `sink` in
/// `chunk_size`-sized pieces. Returns when FFmpeg exits.
///
/// **Real-time pacing:** the `-re` flag tells FFmpeg to read the input
/// at its native frame rate, so a 3-minute song takes 3 wall-clock
/// minutes to pump. Without it, FFmpeg encodes + writes as fast as
/// the CPU + pipe allow — bytes queue up in `sink`, the producer
/// races ahead, listeners hear delayed content, and the idle gaps
/// between back-to-back pumps trigger Icecast's `source-timeout`.
/// With `-re` the consumer naturally back-pressures the upstream
/// channel and listeners hear segments at the right pace.
///
/// **Format normalisation (24-bit / 48 kHz / stereo):** every call
/// spawns a fresh ffmpeg → fresh Ogg stream. When chained on the wire,
/// listener decoders see "stream A ends, stream B begins" — and FLAC
/// decoders explicitly refuse mid-chain changes in bits-per-sample
/// (`switching bps mid-stream is not supported`). The Kokoro TTS
/// segments are 16-bit / 24 kHz / mono; music can be 16- or 24-bit /
/// 44.1 or 48 kHz / stereo. Re-encoding every segment to a fixed
/// 24-bit / 48 kHz / stereo FLAC means every chained Ogg page has
/// identical headers and decoders glide right through. FLAC is
/// lossless, so the re-encode is too — bit-depth upconversion 16→24
/// just zero-pads, mono→stereo duplicates, and 24/44.1 → 24/48 kHz
/// resampling uses soxr (transparent for radio listening). The only
/// real cost is 96/192 kHz hi-res sources getting downsampled to 48
/// kHz; bringing those back losslessly would mean per-listener
/// negotiation, which Icecast doesn't do.
pub async fn pump_to_icecast(
    flac_path: &Path,
    sink: &mpsc::Sender<Vec<u8>>,
    chunk_size: usize,
) -> Result<(), AudioError> {
    let mut child = Command::new(ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-i",
            flac_path.to_string_lossy().as_ref(),
            // Normalise every segment to one stable format so chained
            // Ogg/FLAC pages have identical headers. See doc comment.
            "-ar",
            "48000",
            "-ac",
            "2",
            "-sample_fmt",
            "s32",
            "-bits_per_raw_sample",
            "24",
            "-c:a",
            "flac",
            "-f",
            "ogg",
            "-",
        ])
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
    // pipe (default ~64KB). Collect for the error report — previously
    // we returned the literal string "ffmpeg pump failed" with no
    // signal as to what actually went wrong.
    let stderr = child.stderr.take().expect("ffmpeg stderr");
    let stderr_handle: JoinHandle<Vec<u8>> = tokio::spawn(async move {
        let mut buf = Vec::with_capacity(4096);
        let mut reader = stderr;
        let _ = reader.read_to_end(&mut buf).await;
        buf
    });

    let path_for_log = flac_path.to_string_lossy().into_owned();
    debug!(path = %path_for_log, chunk_size, "pump: ffmpeg started");

    let mut buf = vec![0u8; chunk_size.max(4096)];
    let mut total_bytes: u64 = 0;
    let mut chunks: u64 = 0;
    loop {
        let n = stdout.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if sink.send(buf[..n].to_vec()).await.is_err() {
            // downstream closed — abort the ffmpeg child to clean up.
            let _ = child.kill().await;
            break;
        }
        total_bytes += n as u64;
        chunks += 1;
        // Periodic heartbeat so a stalled run is obvious in the logs.
        // Every ~1 MiB pumped.
        if total_bytes.is_multiple_of(1 << 20) || chunks == 1 {
            debug!(
                path = %path_for_log,
                bytes = total_bytes,
                chunks,
                "pump: progress"
            );
        }
    }
    let status = child.wait().await?;
    let stderr_bytes = stderr_handle.await.unwrap_or_default();
    let stderr_text = String::from_utf8_lossy(&stderr_bytes).into_owned();
    info!(
        path = %path_for_log,
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

    /// Both subprocess error paths (binary missing, binary fails) are
    /// asserted in a single test so the shared `FFMPEG_BIN` env var
    /// doesn't race against itself when `cargo test` parallelizes.
    #[tokio::test]
    async fn surfaces_subprocess_errors() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("dummy.flac");
        std::fs::write(&p, b"not-really-flac").unwrap();
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(1);

        std::env::set_var("FFMPEG_BIN", "/nope/nope/nope/ffmpeg");
        let err = pump_to_icecast(&p, &tx, 1024).await.unwrap_err();
        assert!(matches!(err, AudioError::FfmpegMissing));

        std::env::set_var("FFMPEG_BIN", "/bin/false");
        let err = pump_to_icecast(&p, &tx, 1024).await.unwrap_err();
        assert!(matches!(err, AudioError::FfmpegFailed { .. }));

        std::env::remove_var("FFMPEG_BIN");
    }
}
