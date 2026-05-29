//! Skill trait and shared types.

use crate::audio::AudioProcessor;
use crate::config::Persona;
use crate::feeds::FeedSnapshot;
use crate::library::Track;
use crate::llm::{LlmError, LlmRouter};
use crate::text::tts_safe;
use crate::tts::{KokoroError, TtsEngine};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

/// One past entry the host can refer back to. The producer keeps a
/// rolling window of these and hands them to every skill so the host
/// sounds like they're running a show, not reading detached cards.
#[derive(Debug, Clone)]
pub enum RecentSegment {
    /// A track that played.
    Track {
        artist: String,
        title: String,
        genre: Option<String>,
    },
    /// A spoken segment that aired. `script_preview` is the first ~120
    /// chars of what the host said — enough for the LLM to avoid
    /// repeating itself.
    Spoken {
        kind: &'static str,
        script_preview: String,
    },
}

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
    /// Most-recent-last. Producer maintains; skills read only.
    pub recent: Vec<RecentSegment>,
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
///
/// Every script is run through [`tts_safe`] before it reaches Kokoro,
/// stripping any Markdown, stage directions, or bullet markers the LLM
/// reflexively emits. Without this Kokoro pronounces every `*` and
/// `[chuckles]` literally and the station sounds broken.
pub(crate) async fn render_segment(
    rt: &SkillRuntime,
    persona: &Persona,
    script: String,
    segment_type: &'static str,
) -> Result<SkillOutput, SkillError> {
    // Strip markup, then layer in IPA overrides for known names so the
    // misaki tokenizer says "Asake" right. Order matters: tts_safe eats
    // brackets, so pronunciations have to run after it.
    let clean = tts_safe(&script);
    let voiced = crate::text::apply_pronunciations(&clean, &persona.host.pronunciations);
    let wav = rt
        .tts
        .render(
            &voiced,
            &persona.host.voice_model,
            persona.host.audio.speech_speed,
            &rt.audio.temp_dir,
        )
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
        text: clean,
    })
}

/// Build the system prompt for a skill call.
///
/// Sections in order:
///   1. Identity + tone (one sentence).
///   2. Daypart overlay (only when active — keeps off-hours personas tight).
///   3. Voice signatures (personality block — only emitted when populated).
///   4. **Kokoro output rules** — concrete failure-mode examples. The LLM
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

    // Personality block — sampled per call so the LLM doesn't reflex
    // into the same opener every segment. Openers/closers pick one;
    // catchphrases + inside_jokes show a small subset. recurring_bits
    // stays in full (those define character, sampling makes the host
    // feel inconsistent across segments).
    use rand::seq::{IteratorRandom, SliceRandom};
    let p = &persona.host.personality;
    let mut rng = rand::thread_rng();
    let mut quirks: Vec<String> = Vec::with_capacity(5);
    if let Some(o) = p.signature_openers.choose(&mut rng) {
        quirks.push(format!("opener: {o}"));
    }
    if let Some(c) = p.signature_closers.choose(&mut rng) {
        quirks.push(format!("closer: {c}"));
    }
    if !p.recurring_bits.is_empty() {
        quirks.push(format!("running themes: {}", p.recurring_bits.join(" · ")));
    }
    if !p.catchphrases.is_empty() {
        let picks: Vec<&String> = p.catchphrases.iter().choose_multiple(&mut rng, 3);
        let joined = picks
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        quirks.push(format!("catchphrases (rotation): {joined}"));
    }
    if !p.inside_jokes.is_empty() {
        let picks: Vec<&String> = p.inside_jokes.iter().choose_multiple(&mut rng, 2);
        let joined = picks
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        quirks.push(format!("show callbacks (rotation): {joined}"));
    }
    if !quirks.is_empty() {
        out.push_str("\n\nVoice signatures (use naturally, don't force):");
        for q in quirks {
            out.push_str(&format!("\n- {q}"));
        }
    }

    // Delivery notes — script cadence/shape cues. Kokoro's prosody
    // range is narrow; the punctuation and sentence shape the LLM
    // writes is most of what listeners hear, so this section lives
    // close to the output rules below.
    if let Some(d) = p.delivery_notes.as_deref() {
        out.push_str(&format!(
            "\n\nDelivery — shape the script so Kokoro reads it right:\n{}",
            d.trim()
        ));
    }

    // Kokoro-specific output rules. Wording is deliberately literal —
    // the LLM responds to concrete examples better than abstract advice.
    // Kokoro's prosody range is narrow, so punctuation is the steering
    // wheel: the LLM has to spell rhythm into the text or the read goes
    // flat.
    out.push_str(
        "\n\nOUTPUT — read aloud by Kokoro text-to-speech.\n\
         Plain prose only. Do NOT use any of these characters: * _ ` # [ ] < >\n\
         If you write '*' Kokoro literally says the word \"asterisk\".\n\
         Do not write **bold**, _italic_, headings, or bullet lists. Work emphasis into the sentence.\n\
         \n\
         PUNCTUATION = PACING. Kokoro reads exactly what you give it; \
         vary the rhythm or it'll sound monotone:\n\
         - comma ','  → short beat, ~200ms (use generously).\n\
         - period '.' → full stop, ~400ms. Use short sentences for snap.\n\
         - em-dash '—' → longest in-line pause, ~500ms. Use for a dramatic catch.\n\
         - ellipsis '...' → trailing-off pause, slightly slower decay.\n\
         - question mark '?' → rising intonation. Use to lift a line.\n\
         - exclamation '!' → punchy emphasis. Don't overuse — once per script max.\n\
         - semicolons get read flat; prefer a period + new sentence.\n\
         \n\
         RHYTHM — vary sentence length. A monotone read kills the vibe. Pattern:\n\
         - one short stab (3-5 words). One longer thought, riding the comma, \
         taking its time. Then a tight close.\n\
         - Don't write one long run-on. Don't write five short ones in a row either.\n\
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
         - 'Mr. Davis' → 'Mister Davis'. 'Dr. John' → 'Doctor John'. 'St. Louis' → 'Saint Louis'.",
    );
    out
}

/// Render `recent` into a short block to splice into a skill's user
/// prompt. Empty when there's nothing to say. When non-empty, ends
/// with an explicit continuity rule so the LLM stops re-introducing
/// the station every segment ("Welcome to KFLT..." back-to-back is
/// the classic failure mode).
pub(crate) fn recent_context_block(recent: &[RecentSegment]) -> String {
    if recent.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\nWhat just happened on the show (most recent last):");
    for r in recent {
        match r {
            RecentSegment::Track {
                artist,
                title,
                genre,
            } => {
                let genre_hint = genre
                    .as_deref()
                    .map(|g| format!(" [{g}]"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "\n  - played: \"{title}\" by {artist}{genre_hint}"
                ));
            }
            RecentSegment::Spoken {
                kind,
                script_preview,
            } => {
                out.push_str(&format!("\n  - {kind} said: {script_preview}"));
            }
        }
    }
    out.push_str(
        "\n\nContinuity — you're mid-show, not re-opening it. If the previous segment \
         already said the callsign, your name, or the genre tagline, don't repeat \
         them here. Pick up where you left off.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Host, HostAudio, Personality, Programming, SkillToggles, Stream};

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
                programming: Programming::default(),
                personality: Personality::default(),
                pronunciations: Default::default(),
                audio: HostAudio {
                    eq_profile: None,
                    room_tone: false,
                    loudness_target: -14.0,
                    speech_speed: 1.0,
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
            recent: Vec::new(),
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

    /// Concrete per-punctuation pacing examples and a rhythm note are
    /// what keep Kokoro from reading flat. Vague "speak naturally" got
    /// us a monotone — these are non-negotiable.
    #[test]
    fn system_prompt_explains_each_punctuation_mark() {
        let p = system_prompt(&ctx());
        assert!(p.contains("PUNCTUATION = PACING"));
        // Each major punctuation char gets a concrete cue.
        for needle in ["comma ','", "period '.'", "em-dash '—'", "ellipsis '...'"] {
            assert!(p.contains(needle), "missing pacing rule for {needle}: {p}");
        }
        assert!(p.contains("RHYTHM"), "rhythm guidance missing: {p}");
    }

    #[test]
    fn system_prompt_weaves_in_personality_when_set() {
        // Single-element lists make the sampling deterministic so the
        // test can assert exact strings; rotation behaviour is exercised
        // by `system_prompt_rotates_openers_across_calls` below.
        let mut c = ctx();
        c.persona.host.personality = Personality {
            signature_openers: vec!["Donna here on KFLT.".into()],
            signature_closers: vec!["Stay smooth.".into()],
            recurring_bits: vec!["I knew Mingus when he was just Chuck.".into()],
            catchphrases: vec!["sugar".into()],
            inside_jokes: vec!["Studio One sessions".into()],
            delivery_notes: Some("Sparse. Short stabs. Em-dashes for the catches.".into()),
        };
        let s = system_prompt(&c);
        assert!(s.contains("Voice signatures"));
        assert!(s.contains("opener: Donna here on KFLT."));
        assert!(s.contains("closer: Stay smooth."));
        assert!(s.contains("running themes:") && s.contains("Mingus"));
        assert!(s.contains("catchphrases") && s.contains("sugar"));
        assert!(
            s.contains("Delivery") && s.contains("Em-dashes"),
            "delivery notes missing: {s}"
        );
        assert!(s.contains("show callbacks") && s.contains("Studio One"));
    }

    /// Sampling: across many calls, the prompt must surface multiple
    /// distinct openers — proves the rotation isn't degenerate. Five
    /// openers, sample over 50 calls, demand at least 3 distinct.
    #[test]
    fn system_prompt_rotates_openers_across_calls() {
        let mut c = ctx();
        c.persona.host.personality = Personality {
            signature_openers: vec![
                "Alpha line.".into(),
                "Bravo line.".into(),
                "Charlie line.".into(),
                "Delta line.".into(),
                "Echo line.".into(),
            ],
            ..Default::default()
        };
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for _ in 0..50 {
            let s = system_prompt(&c);
            for needle in ["Alpha", "Bravo", "Charlie", "Delta", "Echo"] {
                if s.contains(needle) {
                    seen.insert(needle.to_string());
                }
            }
        }
        assert!(
            seen.len() >= 3,
            "rotation should surface ≥3 distinct openers across 50 calls, saw {seen:?}"
        );
    }

    /// Sampling cap: catchphrases get a small subset (~3) per call,
    /// not the full list. With 10 catchphrases configured, any single
    /// prompt should contain fewer than 10.
    #[test]
    fn system_prompt_samples_subset_of_catchphrases() {
        let mut c = ctx();
        c.persona.host.personality = Personality {
            catchphrases: (0..10).map(|i| format!("cp_{i}")).collect(),
            ..Default::default()
        };
        let s = system_prompt(&c);
        let hits = (0..10).filter(|i| s.contains(&format!("cp_{i}"))).count();
        assert!(
            (1..10).contains(&hits),
            "expected partial subset of catchphrases (1..10), got {hits} hits"
        );
    }

    #[test]
    fn system_prompt_skips_empty_personality_fields() {
        // Default persona has no quirks — the section header itself
        // should be absent so we don't waste tokens on empty markup.
        let s = system_prompt(&ctx());
        assert!(!s.contains("Voice signatures"));
        assert!(!s.contains("opener:"));
        assert!(!s.contains("catchphrases:"));
    }

    #[test]
    fn recent_context_block_empty_when_nothing_recent() {
        assert!(recent_context_block(&[]).is_empty());
    }

    #[test]
    fn recent_context_block_lists_tracks_and_segments() {
        let recent = vec![
            RecentSegment::Track {
                artist: "Miles Davis".into(),
                title: "So What".into(),
                genre: Some("Jazz".into()),
            },
            RecentSegment::Spoken {
                kind: "track_intro",
                script_preview: "Coming up next, the legendary Miles.".into(),
            },
        ];
        let b = recent_context_block(&recent);
        assert!(b.contains("Miles Davis"));
        assert!(b.contains("So What"));
        assert!(b.contains("[Jazz]"));
        assert!(b.contains("track_intro said"));
        assert!(b.contains("Coming up next"));
    }
}
