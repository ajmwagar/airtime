//! `weather` — quick local conditions read.

use super::base::{render_segment, system_prompt, Skill, SkillContext, SkillError, SkillOutput, SkillRuntime};
use async_trait::async_trait;

pub struct WeatherSkill;

#[async_trait]
impl Skill for WeatherSkill {
    fn name(&self) -> &'static str {
        "weather"
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
        let script = rt.llm.complete(&system, &user, &cfg.llm_backend).await?;
        render_segment(rt, &ctx.persona, script, "weather").await
    }
}
