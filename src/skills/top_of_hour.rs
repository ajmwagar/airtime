//! `top_of_hour` — DJ reads the news headlines and station ID at the top of the hour.

use super::base::{
    render_segment, system_prompt, Skill, SkillContext, SkillError, SkillOutput, SkillRuntime,
    SKILL_SCORE_REQUIRED, SKILL_SCORE_SUPPRESSED,
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

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError> {
        let cfg = ctx.persona.skill_config(self.name());
        let system = system_prompt(&ctx.persona);
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
            "Top of the hour. Give the station ID ({callsign}) and read these headlines in your \
             own voice. Don't quote them verbatim — paraphrase, add brief commentary. \
             Max {max} words. Spoken naturally.\n\nHeadlines:\n{headlines}",
            callsign = ctx.persona.host.callsign,
            max = cfg.max_words,
        );
        let script = rt.llm.complete(&system, &user, &cfg.llm_backend).await?;
        render_segment(rt, &ctx.persona, script, "top_of_hour").await
    }
}
