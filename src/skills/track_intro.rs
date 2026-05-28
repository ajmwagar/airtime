//! `track_intro` — DJ introduces the upcoming track.

use super::base::{
    render_segment, system_prompt, Skill, SkillContext, SkillError, SkillOutput, SkillRuntime,
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
        let system = system_prompt(&ctx.persona);
        let year = track
            .year
            .map(|y| y.to_string())
            .unwrap_or_else(|| "unknown".into());
        let user = format!(
            "Next track: \"{title}\" by {artist} ({year}, {album}).\n\
             Write a track intro. Max {max} words. Spoken naturally.",
            title = track.title,
            artist = track.artist,
            album = track.album,
            max = cfg.max_words,
        );
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "track_intro").await
    }
}
