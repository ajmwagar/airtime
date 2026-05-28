//! `traffic` — local incident summary read.

use super::base::{render_segment, system_prompt, Skill, SkillContext, SkillError, SkillOutput, SkillRuntime};
use async_trait::async_trait;

pub struct TrafficSkill;

#[async_trait]
impl Skill for TrafficSkill {
    fn name(&self) -> &'static str {
        "traffic"
    }

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError> {
        let cfg = ctx.persona.skill_config(self.name());
        let system = system_prompt(&ctx.persona);
        let user = match ctx.feeds.traffic.as_ref() {
            Some(t) => format!(
                "Traffic report: {summary} ({n} incidents in the metro). \
                 Give a brief traffic read in your voice. Max {max} words. Spoken naturally.",
                summary = t.summary,
                n = t.incidents,
                max = cfg.max_words,
            ),
            None => format!(
                "No traffic data right now. Keep it brief and honest. Max {} words.",
                cfg.max_words
            ),
        };
        let script = rt.llm.complete(&system, &user, &cfg.llm_backend).await?;
        render_segment(rt, &ctx.persona, script, "traffic").await
    }
}
