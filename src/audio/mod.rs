//! Audio processing helpers.
//!
//! Everything here shells out to FFmpeg — we keep the Rust side thin and
//! testable. The pipeline is lossless: PCM → FLAC, always.

pub mod eq;
pub mod normalize;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("ffmpeg not found on PATH (set FFMPEG_BIN or install ffmpeg)")]
    FfmpegMissing,
    #[error("ffmpeg failed (exit {code}): {stderr}")]
    FfmpegFailed { code: i32, stderr: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ffprobe could not parse duration: {0}")]
    DurationParse(String),
}

/// Where the FFmpeg binary lives. Honour `FFMPEG_BIN` for tests/CI that
/// stub it out, otherwise fall back to `ffmpeg` on PATH.
pub fn ffmpeg_bin() -> String {
    std::env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into())
}

pub fn ffprobe_bin() -> String {
    std::env::var("FFPROBE_BIN").unwrap_or_else(|_| "ffprobe".into())
}

pub async fn probe_duration_ms(path: impl AsRef<Path>) -> Result<u64, AudioError> {
    let output = Command::new(ffprobe_bin())
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path.as_ref())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AudioError::FfmpegMissing
            } else {
                AudioError::Io(e)
            }
        })?;
    if !output.status.success() {
        return Err(AudioError::FfmpegFailed {
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let seconds: f64 = raw
        .trim()
        .parse()
        .map_err(|_| AudioError::DurationParse(raw.into_owned()))?;
    Ok((seconds * 1000.0).round() as u64)
}

/// Full post-render audio pipeline: loudnorm → optional EQ → FLAC.
///
/// Returns the path to the processed FLAC file. The pipeline is lossless;
/// the input is expected to be PCM WAV (e.g. straight out of Kokoro) and
/// the output is FLAC at the source sample rate.
pub struct AudioProcessor {
    pub temp_dir: PathBuf,
    pub loudness_target: f64,
    pub true_peak: f64,
}

impl AudioProcessor {
    pub fn new(temp_dir: PathBuf, loudness_target: f64, true_peak: f64) -> Self {
        Self {
            temp_dir,
            loudness_target,
            true_peak,
        }
    }

    /// Generate a silent FLAC at `temp_dir/silence-{secs}s.flac` if it
    /// doesn't already exist, returning the path. Used by the consumer
    /// as Icecast keepalive during idle periods (empty library,
    /// in-flight skill render). Mono, 24 kHz — matches Kokoro's
    /// output rate so the silent stream interleaves cleanly with
    /// real TTS segments.
    pub async fn ensure_silence(&self, duration_secs: u64) -> Result<PathBuf, AudioError> {
        tokio::fs::create_dir_all(&self.temp_dir).await?;
        let out = self.temp_dir.join(format!("silence-{duration_secs}s.flac"));
        if tokio::fs::metadata(&out).await.is_ok() {
            return Ok(out);
        }
        run_ffmpeg(&[
            "-y",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=24000:cl=mono",
            "-t",
            &duration_secs.to_string(),
            "-c:a",
            "flac",
            out.to_string_lossy().as_ref(),
        ])
        .await?;
        Ok(out)
    }

    pub async fn process(
        &self,
        input: &Path,
        eq_profile: Option<&str>,
    ) -> Result<PathBuf, AudioError> {
        tokio::fs::create_dir_all(&self.temp_dir).await?;
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "segment".into());
        let out = self.temp_dir.join(format!("{stem}.processed.ogg"));

        let mut filter = format!(
            "loudnorm=I={i}:TP={tp}:LRA=11",
            i = self.loudness_target,
            tp = self.true_peak
        );
        if let Some(profile) = eq_profile {
            if let Some(eq_chain) = eq::profile_filter(profile) {
                filter.push(',');
                filter.push_str(eq_chain);
            }
        }

        run_ffmpeg(&[
            "-y",
            "-i",
            input.to_string_lossy().as_ref(),
            "-af",
            &filter,
            "-c:a",
            "flac",
            "-f",
            "ogg",
            out.to_string_lossy().as_ref(),
        ])
        .await?;

        Ok(out)
    }
}

pub(crate) async fn run_ffmpeg(args: &[&str]) -> Result<(), AudioError> {
    let output = Command::new(ffmpeg_bin())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AudioError::FfmpegMissing
            } else {
                AudioError::Io(e)
            }
        })?;
    if !output.status.success() {
        return Err(AudioError::FfmpegFailed {
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // FFmpeg invocation paths are exercised in `mixer::tests` and the
    // skill integration tests. Env-var-based overrides are intentionally
    // tested there (serialized) rather than here, to avoid cross-test
    // env-var races under `cargo test`'s default parallel runner.
}
