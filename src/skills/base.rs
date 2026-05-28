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
    /// Active daypart's `extra_tone`, if any — appended to the system
    /// prompt so the host sounds different at 2 AM than at 2 PM. The
    /// producer fills this from `host.current_daypart(hour)`.
    pub daypart_tone: Option<String>,
    /// Local wall-clock hour (0..24). Skills with hour-specific
    /// behaviour read this instead of pulling their own clock.
    pub hour: u32,
    /// Local wall-clock minute (0..60).
    pub minute: u32,
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

    /// Opt in: this skill is supposed to fire **at most once per hour**
    /// at a specific window (typically a `time_score` REQUIRED slot).
    /// If a long track straddles the window and the skill never gets
    /// a slot, the scheduler treats it as overdue and gives it makeup
    /// priority at the next break — instead of skipping the hour
    /// silently. See `SegmentScheduler::overdue_anchor_skill`.
    fn must_fire_per_hour(&self) -> bool {
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

/// Build the system prompt for a skill call.
///
/// Three sections in order:
///   1. Identity + tone (one sentence).
///   2. Daypart overlay (only when active — keeps off-hours personas tight).
///   3. **Kokoro output rules** — concrete failure-mode examples. The LLM
///      otherwise reflex-emits Markdown and ALL-CAPS abbreviations, and
///      Kokoro then pronounces every `*` as "asterisk" and `RPM` as
///      "are pee em". Generic "speak naturally" instructions don't
///      survive — the examples do.
pub(crate) fn system_prompt(ctx: &SkillContext) -> String {
    let persona = &ctx.persona;
    let mut out = format!(
        "You are {name} on {callsign}, the {era} {genres} station. {tone}",
        name = persona.host.name,
        callsign = persona.host.callsign,
        era = persona.host.era,
        genres = persona.host.genre.join(", "),
        tone = persona.host.tone_prompt.trim(),
    );

    if let Some(dp) = ctx.daypart_tone.as_deref() {
        out.push_str(&format!("\n\nRight now: {}", dp.trim()));
    }

    // Kokoro-specific output rules. Wording is deliberately literal —
    // the LLM responds to concrete examples better than abstract advice.
    out.push_str(
        "\n\nOUTPUT — read aloud by Kokoro text-to-speech.\n\
         Plain prose only. Do NOT use any of these characters: * _ ` # [ ] < >\n\
         If you write '*' Kokoro literally says the word \"asterisk\".\n\
         Do not write **bold**, _italic_, headings, or bullet lists. Work emphasis into the sentence.\n\
         \n\
         Spell out ALL-CAPS abbreviations the way you'd say them:\n\
         - 'RPM' → 'revolutions per minute' (or 'R, P, M' if you mean the letters).\n\
         - 'NPR' → 'N, P, R'.\n\
         - Filler sounds like 'mhm' or 'uh-huh' — just say 'mm-hmm' written as the syllable, or skip them.\n\
         \n\
         Spell numbers as words:\n\
         - '1959' → 'nineteen fifty-nine'.\n\
         - '60s' → 'sixties'.\n\
         - '$10' → 'ten dollars'.\n\
         \n\
         Spell honorifics:\n\
         - 'Mr. Davis' → 'Mister Davis'. 'Dr. John' → 'Doctor John'. 'St. Louis' → 'Saint Louis'.\n\
         \n\
         Punctuate for breath: comma for a beat, period to stop, em-dash '—' for a longer pause.",
    );
    out
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
                tracks_per_break: 3,
                units: "imperial".into(),
                dayparts: Vec::new(),
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

    pub(crate) fn ctx() -> SkillContext {
        SkillContext {
            persona: persona(),
            track: None,
            feeds: FeedSnapshot::default(),
            daypart_tone: None,
            hour: 12,
            minute: 0,
        }
    }

    #[test]
    fn system_prompt_includes_identity() {
        let p = system_prompt(&ctx());
        assert!(p.contains("Donna"));
        assert!(p.contains("KFLT"));
        assert!(p.contains("1960s"));
        assert!(p.contains("jazz, soul"));
    }

    /// The Kokoro output-rules section is non-negotiable: every prompt
    /// must carry the concrete examples. Vague rules ("speak naturally")
    /// didn't survive contact with the actual model — Donna kept reading
    /// "asterisk RPM asterisk" out loud.
    #[test]
    fn system_prompt_warns_about_asterisks_and_caps() {
        let p = system_prompt(&ctx());
        assert!(p.contains("asterisk"));
        assert!(p.contains("RPM"));
        assert!(p.contains("revolutions per minute"));
    }

    #[test]
    fn system_prompt_gives_number_and_honorific_examples() {
        let p = system_prompt(&ctx());
        assert!(p.contains("nineteen fifty-nine"));
        assert!(p.contains("Mister"));
    }

    #[test]
    fn system_prompt_includes_daypart_tone_when_set() {
        let mut c = ctx();
        c.daypart_tone = Some("Late-night driver vibe.".into());
        let p = system_prompt(&c);
        assert!(p.contains("Right now: Late-night driver vibe"));
    }

    #[test]
    fn system_prompt_skips_daypart_when_unset() {
        let p = system_prompt(&ctx());
        assert!(!p.contains("Right now:"));
    }
}
