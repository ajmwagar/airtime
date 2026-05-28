//! Kokoro TTS subprocess wrapper.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum KokoroError {
    #[error("kokoro binary not found at {0} — set [kokoro].binary or KOKORO_BIN")]
    BinaryMissing(PathBuf),
    #[error("kokoro failed (exit {code}): {stderr}")]
    Failed { code: i32, stderr: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub trait TtsEngine: Send + Sync {
    /// Render `text` to a WAV file using `voice_model`. Returns the path.
    async fn render(
        &self,
        text: &str,
        voice_model: &str,
        out_dir: &Path,
    ) -> Result<PathBuf, KokoroError>;
}

pub struct KokoroTts {
    pub binary: PathBuf,
    pub model_path: PathBuf,
    pub voices_path: PathBuf,
}

impl KokoroTts {
    pub fn new(
        binary: impl Into<PathBuf>,
        model_path: impl Into<PathBuf>,
        voices_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            binary: binary.into(),
            model_path: model_path.into(),
            voices_path: voices_path.into(),
        }
    }
}

#[async_trait]
impl TtsEngine for KokoroTts {
    async fn render(
        &self,
        text: &str,
        voice_model: &str,
        out_dir: &Path,
    ) -> Result<PathBuf, KokoroError> {
        tokio::fs::create_dir_all(out_dir).await?;
        let stem = format!(
            "tts-{:x}",
            // millisecond timestamp as a hex stem is plenty for uniqueness.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );
        let out = out_dir.join(format!("{stem}.wav"));

        let mut child = Command::new(&self.binary)
            .args([
                "--model",
                self.model_path.to_string_lossy().as_ref(),
                "--voices",
                self.voices_path.to_string_lossy().as_ref(),
                "--voice",
                voice_model,
                "--out",
                out.to_string_lossy().as_ref(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    KokoroError::BinaryMissing(self.binary.clone())
                } else {
                    KokoroError::Io(e)
                }
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes()).await?;
            stdin.flush().await?;
        }
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err(KokoroError::Failed {
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn missing_binary_returns_typed_error() {
        let tmp = TempDir::new().unwrap();
        let tts = KokoroTts::new(
            "/definitely/not/a/real/path/kokoro",
            tmp.path().join("model.onnx"),
            tmp.path().join("voices.bin"),
        );
        let err = tts
            .render("hello", "af_heart", tmp.path())
            .await
            .unwrap_err();
        assert!(matches!(err, KokoroError::BinaryMissing(_)));
    }

    /// Use `/bin/true` as a stand-in for a working Kokoro binary so we can
    /// verify the full subprocess plumbing without actually running TTS.
    /// `/bin/true` succeeds without producing the output file, so we
    /// expect the call to succeed (we don't validate the output file
    /// here — that's the wrapper contract, not ours).
    #[tokio::test]
    async fn invokes_subprocess_when_binary_exists() {
        let tmp = TempDir::new().unwrap();
        let tts = KokoroTts::new(
            "/bin/true",
            tmp.path().join("model.onnx"),
            tmp.path().join("voices.bin"),
        );
        let out = tts.render("hi", "af_heart", tmp.path()).await.unwrap();
        assert!(out.to_string_lossy().ends_with(".wav"));
    }
}
