//! `track_outro` — DJ wraps up the track that just played.

use super::base::{
    recent_context_block, render_segment, system_prompt, Skill, SkillContext, SkillError,
    SkillOutput, SkillRuntime,
};
use async_trait::async_trait;

pub struct TrackOutroSkill;

#[async_trait]
impl Skill for TrackOutroSkill {
    fn name(&self) -> &'static str {
        "track_outro"
    }

    fn needs_track(&self) -> bool {
        true
    }

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError> {
        let track = ctx.track.as_ref().ok_or_else(|| {
            SkillError::Llm(crate::llm::LlmError::Malformed(
                "track_outro requires a track".into(),
            ))
        })?;
        let cfg = ctx.persona.skill_config(self.name());
        let system = system_prompt(&ctx.persona);
        let user = format!(
            "Outro that track (\"{title}\" — {artist}) in max {max} words. Brief reflection or callback.{recent}",
            max = cfg.max_words,
            title = track.title,
            artist = track.artist,
            recent = recent_context_block(&ctx.recent),
        );
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "track_outro").await
    }
}
