//! `weather` — quick local conditions read.

use super::base::{
    recent_context_block, render_segment, system_prompt, Skill, SkillContext, SkillError,
    SkillOutput, SkillRuntime, SKILL_SCORE_BASELINE, SKILL_SCORE_PREFERRED,
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
        let system = system_prompt(ctx);
        let user = match ctx.feeds.weather.as_ref() {
            Some(w) => {
                // Open-Meteo gives us metric (°C, km/h). Convert and
                // spell out the unit name based on the persona's
                // `units` setting. Kokoro reads "kph" as "K, P, H"
                // and "mph" as "M, P, H" — both bad — so always
                // write the words out.
                let (temp_str, temp_unit, wind_str, wind_unit) = if ctx.persona.host.imperial() {
                    (
                        format!(
                            "{:.0}",
                            crate::config::celsius_to_fahrenheit(w.temperature_c)
                        ),
                        "Fahrenheit",
                        format!("{:.0}", crate::config::kph_to_mph(w.wind_kph)),
                        "miles per hour",
                    )
                } else {
                    (
                        format!("{:.0}", w.temperature_c),
                        "Celsius",
                        format!("{:.0}", w.wind_kph),
                        "kilometers per hour",
                    )
                };
                format!(
                    "Quick weather read. Right now: {temp_str} degrees {temp_unit}, \
                     {cond}, wind {wind_str} {wind_unit}. One sentence, two max. \
                     Max {max} words.{recent}",
                    cond = w.conditions,
                    max = cfg.max_words,
                    recent = recent_context_block(&ctx.recent),
                )
            }
            None => format!(
                "Weather data's out — say so briefly, no made-up numbers. Max {} words.{}",
                cfg.max_words,
                recent_context_block(&ctx.recent),
            ),
        };
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "weather").await
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{celsius_to_fahrenheit, kph_to_mph};

    /// Conversions have to be right: the whole point of
    /// persona-configurable units is the right number lands in the
    /// LLM prompt. 20 °C = 68 °F. 100 km/h ≈ 62 mph.
    #[test]
    fn celsius_to_fahrenheit_conversion() {
        assert_eq!(celsius_to_fahrenheit(0.0).round() as i32, 32);
        assert_eq!(celsius_to_fahrenheit(20.0).round() as i32, 68);
        assert_eq!(celsius_to_fahrenheit(100.0).round() as i32, 212);
        assert_eq!(celsius_to_fahrenheit(-40.0).round() as i32, -40);
    }

    #[test]
    fn kph_to_mph_conversion() {
        assert_eq!(kph_to_mph(0.0).round() as i32, 0);
        assert_eq!(kph_to_mph(100.0).round() as i32, 62);
        assert_eq!(kph_to_mph(50.0).round() as i32, 31);
    }
}
