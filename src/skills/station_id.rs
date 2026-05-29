//! `station_id` — short imager. "You're listening to KFLT…"

use super::base::{
    recent_context_block, render_segment, system_prompt, Skill, SkillContext, SkillError,
    SkillOutput, SkillRuntime, SKILL_SCORE_BASELINE, SKILL_SCORE_PREFERRED,
};
use async_trait::async_trait;

pub struct StationIdSkill;

#[async_trait]
impl Skill for StationIdSkill {
    fn name(&self) -> &'static str {
        "station_id"
    }

    /// Drop near the quarter-hour marks (:15, :30, :45) — classic radio
    /// imager cadence. Still eligible at baseline elsewhere.
    fn time_score(&self, minute: u32) -> i32 {
        match minute {
            14..=16 | 29..=31 | 44..=46 => SKILL_SCORE_PREFERRED,
            _ => SKILL_SCORE_BASELINE,
        }
    }

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError> {
        let cfg = ctx.persona.skill_config(self.name());
        let system = system_prompt(ctx);
        let user = format!(
            "Quick station ID for {callsign} ({genres}). One sharp line — \
             tagline energy, not a paragraph. Max {max} words.{recent}",
            callsign = ctx.persona.host.callsign,
            genres = ctx.persona.host.genre.join(", "),
            max = cfg.max_words,
            recent = recent_context_block(&ctx.recent),
        );
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "station_id").await
    }
}
