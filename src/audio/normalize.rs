//! FFmpeg `loudnorm` wrapper — two-pass measurement then linear apply.
//!
//! For Phase 1 we use the single-pass form (good enough for spoken DJ
//! segments). Two-pass measurement is wired up via `AudioProcessor` if a
//! caller wants it later.

use super::{run_ffmpeg, AudioError};
use std::path::{Path, PathBuf};

pub struct Normalizer {
    pub target_lufs: f64,
    pub true_peak: f64,
}

impl Normalizer {
    pub fn new(target_lufs: f64, true_peak: f64) -> Self {
        Self {
            target_lufs,
            true_peak,
        }
    }

    /// Normalize `input` and write the result to `output`. Output stays
    /// lossless (FLAC).
    pub async fn run(&self, input: &Path, output: &Path) -> Result<PathBuf, AudioError> {
        let filter = format!(
            "loudnorm=I={i}:TP={tp}:LRA=11",
            i = self.target_lufs,
            tp = self.true_peak
        );
        run_ffmpeg(&[
            "-y",
            "-i",
            input.to_string_lossy().as_ref(),
            "-af",
            &filter,
            "-c:a",
            "flac",
            output.to_string_lossy().as_ref(),
        ])
        .await?;
        Ok(output.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_targets() {
        let n = Normalizer::new(-14.0, -1.0);
        assert_eq!(n.target_lufs, -14.0);
        assert_eq!(n.true_peak, -1.0);
    }
}
