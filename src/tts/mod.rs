//! Kokoro TTS subprocess wrapper.
//!
//! Phase 1 invokes a Kokoro CLI binary as a subprocess: text on stdin,
//! WAV on stdout. The binary path and model paths come from settings.
//! Tests use a stub binary (a shell script or `tts_test_double`) by
//! pointing `KOKORO_BIN` at it.

pub mod kokoro;

pub use kokoro::{KokoroError, KokoroTts, TtsEngine};
