//! `track_intro` — DJ introduces the upcoming track.
//!
//! Uses `ctx.recent` for show-aware patter: the host can reference what
//! just played, and when crossing a genre boundary (and the persona has
//! `programming.narrate_transitions = true`) the prompt explicitly
//! cues a segue.

use super::base::{
    recent_context_block, render_segment, system_prompt, RecentSegment, Skill, SkillContext,
    SkillError, SkillOutput, SkillRuntime,
};
use async_trait::async_trait;

pub struct TrackIntroSkill;

#[async_trait]
impl Skill for TrackIntroSkill {
    fn name(&self) -> &'static str {
        "track_intro"
    }

    fn needs_track(&self) -> bool {
        true
    }

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError> {
        let track = match ctx.track.as_ref() {
            Some(t) => t,
            None => {
                return Err(SkillError::Llm(crate::llm::LlmError::Malformed(
                    "track_intro requires a track".into(),
                )))
            }
        };
        let cfg = ctx.persona.skill_config(self.name());
        let system = system_prompt(ctx);
        let year = track
            .year
            .map(|y| y.to_string())
            .unwrap_or_else(|| "unknown".into());

        let transition_hint = if ctx.persona.host.programming.narrate_transitions {
            previous_track_genre(&ctx.recent)
                .filter(|prev_g| {
                    track
                        .genre
                        .as_deref()
                        .map(|g| !g.eq_ignore_ascii_case(prev_g))
                        .unwrap_or(false)
                })
                .map(|prev_g| {
                    format!(
                        " Segue from {prev_g} to {curr_g} — acknowledge the turn.",
                        prev_g = prev_g,
                        curr_g = track.genre.as_deref().unwrap_or("this"),
                    )
                })
                .unwrap_or_default()
        } else {
            String::new()
        };

        let user = format!(
            "Intro this track in max {max} words: \"{title}\" — {artist} ({year}, {album}).{transition}{recent}",
            max = cfg.max_words,
            title = track.title,
            artist = track.artist,
            album = track.album,
            transition = transition_hint,
            recent = recent_context_block(&ctx.recent),
        );
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "track_intro").await
    }
}

/// Find the genre of the most-recently-played track in the recent
/// window, if any. Ignores spoken segments.
fn previous_track_genre(recent: &[RecentSegment]) -> Option<String> {
    for r in recent.iter().rev() {
        if let RecentSegment::Track { genre: Some(g), .. } = r {
            return Some(g.clone());
        }
    }
    None
}
