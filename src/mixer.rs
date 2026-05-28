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
            "-c:a",
            "copy",
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
    let mut buf = vec![0u8; chunk_size.max(4096)];
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
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(AudioError::FfmpegFailed {
            code: status.code().unwrap_or(-1),
            stderr: "ffmpeg pump failed".into(),
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
