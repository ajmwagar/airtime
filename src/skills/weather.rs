//! `weather` — quick local conditions read.

use super::base::{
    render_segment, system_prompt, Skill, SkillContext, SkillError, SkillOutput, SkillRuntime,
    SKILL_SCORE_BASELINE, SKILL_SCORE_PREFERRED,
};
use async_trait::async_trait;

pub struct WeatherSkill;

#[async_trait]
impl Skill for WeatherSkill {
    fn name(&self) -> &'static str {
        "weather"
    }

    /// Prefer the post-news / pre-hour windows where a weather read sits
    /// naturally — around :25 (after a top-of-hour news flow) and :55
    /// (running up to the next top-of-hour).
    fn time_score(&self, minute: u32) -> i32 {
        match minute {
            24..=27 | 54..=58 => SKILL_SCORE_PREFERRED,
            _ => SKILL_SCORE_BASELINE,
        }
    }

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError> {
        let cfg = ctx.persona.skill_config(self.name());
        let system = system_prompt(&ctx.persona);
        let user = match ctx.feeds.weather.as_ref() {
            Some(w) => format!(
                "Weather right now: {temp:.0}°C, {cond}, wind {wind:.0} kph. \
                 Give a short weather read in your voice. Max {max} words. Spoken naturally.",
                temp = w.temperature_c,
                cond = w.conditions,
                wind = w.wind_kph,
                max = cfg.max_words,
            ),
            None => format!(
                "We don't have a fresh weather reading. Improvise a brief, \
                 honest weather mention (without making up numbers). \
                 Max {} words.",
                cfg.max_words
            ),
        };
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "weather").await
    }
}
