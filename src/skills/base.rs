//! Skill trait and shared types.

use crate::audio::AudioProcessor;
use crate::config::Persona;
use crate::feeds::FeedSnapshot;
use crate::library::Track;
use crate::llm::{LlmError, LlmRouter};
use crate::tts::{KokoroError, TtsEngine};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SkillContext {
    pub persona: Persona,
    pub track: Option<Track>,
    pub feeds: FeedSnapshot,
}

#[derive(Debug, Clone)]
pub struct SkillOutput {
    pub audio_path: PathBuf,
    pub duration_ms: u64,
    pub segment_type: &'static str,
    pub text: String,
}

/// Shared runtime handed to every skill. Keeps the skill code free of
/// constructor boilerplate.
#[derive(Clone)]
pub struct SkillRuntime {
    pub llm: Arc<LlmRouter>,
    pub tts: Arc<dyn TtsEngine>,
    pub audio: Arc<AudioProcessor>,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("llm: {0}")]
    Llm(#[from] LlmError),
    #[error("tts: {0}")]
    Tts(#[from] KokoroError),
    #[error("audio: {0}")]
    Audio(#[from] crate::audio::AudioError),
}

#[async_trait]
pub trait Skill: Send + Sync {
    fn name(&self) -> &'static str;

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError>;

    /// Whether this skill needs `ctx.track` to be set (track_intro,
    /// track_outro). The scheduler uses this to gate scheduling.
    fn needs_track(&self) -> bool {
        false
    }
}

/// Common helper that turns the LLM script into a finished, processed
/// FLAC segment. All skills route through this so the audio pipeline
/// stays uniform.
pub(crate) async fn render_segment(
    rt: &SkillRuntime,
    persona: &Persona,
    script: String,
    segment_type: &'static str,
) -> Result<SkillOutput, SkillError> {
    let wav = rt
        .tts
        .render(&script, &persona.host.voice_model, &rt.audio.temp_dir)
        .await?;
    let processed = rt
        .audio
        .process(&wav, persona.host.audio.eq_profile.as_deref())
        .await?;
    let duration_ms = crate::audio::probe_duration_ms(&processed)
        .await
        .unwrap_or(0);
    Ok(SkillOutput {
        audio_path: processed,
        duration_ms,
        segment_type,
        text: script,
    })
}

pub(crate) fn system_prompt(persona: &Persona) -> String {
    format!(
        "You are {name}, a radio DJ on {callsign}.\n\
         Tone: {tone}\n\
         Era/genre: {era} {genres} radio.\n\
         Output spoken radio copy only. No stage directions. No quotes. \
         Natural speech.",
        name = persona.host.name,
        callsign = persona.host.callsign,
        tone = persona.host.tone_prompt.trim(),
        era = persona.host.era,
        genres = persona.host.genre.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Host, HostAudio, SkillToggles, Stream};

    fn persona() -> Persona {
        Persona {
            host: Host {
                name: "Donna".into(),
                callsign: "KFLT".into(),
                era: "1960s".into(),
                genre: vec!["jazz".into(), "soul".into()],
                voice_model: "af_heart".into(),
                tone_prompt: "Warm, sardonic.".into(),
                skills: SkillToggles::default(),
                skill_config: Default::default(),
                audio: HostAudio {
                    eq_profile: None,
                    room_tone: false,
                    loudness_target: -14.0,
                },
            },
            stream: Stream {
                mount: "/donna".into(),
                format: "flac".into(),
                bitrate: None,
            },
        }
    }

    #[test]
    fn system_prompt_includes_identity() {
        let p = system_prompt(&persona());
        assert!(p.contains("Donna"));
        assert!(p.contains("KFLT"));
        assert!(p.contains("1960s"));
        assert!(p.contains("jazz, soul"));
    }
}
