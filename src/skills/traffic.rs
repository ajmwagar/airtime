//! `traffic` — local incident summary read.

use super::base::{
    recent_context_block, render_segment, system_prompt, Skill, SkillContext, SkillError,
    SkillOutput, SkillRuntime, SKILL_SCORE_BASELINE, SKILL_SCORE_PREFERRED,
};
use async_trait::async_trait;

pub struct TrafficSkill;

#[async_trait]
impl Skill for TrafficSkill {
    fn name(&self) -> &'static str {
        "traffic"
    }

    /// Traffic reads pair with the weather windows on radio — :22ish and
    /// :52ish are the canonical "weather and traffic" slots.
    fn time_score(&self, minute: u32) -> i32 {
        match minute {
            21..=24 | 51..=54 => SKILL_SCORE_PREFERRED,
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
        let user = match ctx.feeds.traffic.as_ref() {
            Some(t) => format!(
                "Quick traffic. {summary} ({n} incidents). Punch the headline, \
                 skip the throat-clearing. Max {max} words.{recent}",
                summary = t.summary,
                n = t.incidents,
                max = cfg.max_words,
                recent = recent_context_block(&ctx.recent),
            ),
            None => format!(
                "Traffic data's out — one honest line. Max {} words.{}",
                cfg.max_words,
                recent_context_block(&ctx.recent),
            ),
        };
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "traffic").await
    }
}
