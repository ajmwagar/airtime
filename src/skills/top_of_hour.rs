//! `top_of_hour` — DJ reads the news headlines and station ID at the top of the hour.

use super::base::{
    recent_context_block, render_segment, system_prompt, Skill, SkillContext, SkillError,
    SkillOutput, SkillRuntime, SKILL_SCORE_REQUIRED, SKILL_SCORE_SUPPRESSED,
};
use async_trait::async_trait;

pub struct TopOfHourSkill;

#[async_trait]
impl Skill for TopOfHourSkill {
    fn name(&self) -> &'static str {
        "top_of_hour"
    }

    /// Strongly locked to the first few minutes of the hour — outside
    /// that window we suppress so it doesn't fire in the middle of a set.
    fn time_score(&self, minute: u32) -> i32 {
        if minute < 5 {
            SKILL_SCORE_REQUIRED
        } else {
            SKILL_SCORE_SUPPRESSED
        }
    }

    /// News at the top of the hour is the canonical hour-anchor skill:
    /// if a long track straddles `:00–:04` and we miss the window, the
    /// scheduler should fire us as soon as the track ends instead of
    /// skipping the hour entirely.
    fn must_fire_per_hour(&self) -> bool {
        true
    }

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError> {
        let cfg = ctx.persona.skill_config(self.name());
        let system = system_prompt(ctx);
        let headlines = if ctx.feeds.news.is_empty() {
            "(no headlines available)".to_string()
        } else {
            ctx.feeds
                .news
                .iter()
                .take(5)
                .map(|i| format!("- {}", i.title))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let user = format!(
            "Top of the hour. Quick station mark, then ride these headlines — paraphrase, \
             add a take, don't quote. Max {max} words.\n\n{headlines}{recent}",
            max = cfg.max_words,
            recent = recent_context_block(&ctx.recent),
        );
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "top_of_hour").await
    }
}
