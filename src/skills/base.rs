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

/// Score returned by `Skill::time_score` for skills that are happy to
/// run at any minute of the hour.
pub const SKILL_SCORE_BASELINE: i32 = 10;
/// Skill prefers this slot but isn't strictly tied to it.
pub const SKILL_SCORE_PREFERRED: i32 = 50;
/// Skill is strongly tied to this slot (e.g. `top_of_hour` at minute 0).
pub const SKILL_SCORE_REQUIRED: i32 = 100;
/// Used to suppress a skill at a given minute — the scheduler treats
/// non-positive scores as "not eligible right now".
pub const SKILL_SCORE_SUPPRESSED: i32 = 0;

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

    /// How well this skill fits the current minute of the hour (0..60).
    ///
    /// `SKILL_SCORE_BASELINE` means "fine any time"; the scheduler picks
    /// the highest-scoring eligible skill and falls back to baseline
    /// candidates when nothing scores higher. Returning
    /// `SKILL_SCORE_SUPPRESSED` (or less) means "skip me at this minute".
    fn time_score(&self, _minute: u32) -> i32 {
        SKILL_SCORE_BASELINE
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
                default_llm_backend: None,
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
