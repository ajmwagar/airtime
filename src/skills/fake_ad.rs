//! `fake_ad` — GTA-mode parody ad spot for a fictional product.

use super::base::{
    render_segment, system_prompt, Skill, SkillContext, SkillError, SkillOutput, SkillRuntime,
};
use async_trait::async_trait;

pub struct FakeAdSkill;

#[async_trait]
impl Skill for FakeAdSkill {
    fn name(&self) -> &'static str {
        "fake_ad"
    }

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError> {
        let cfg = ctx.persona.skill_config(self.name());
        let system = system_prompt(&ctx.persona);
        let user = format!(
            "Write a 30-second parody radio ad spot for a fictional product or business in the \
             style of GTA Radio. Be over-the-top, era-appropriate, and slightly absurd. End with \
             a fake phone number or tagline. Max {} words.",
            cfg.max_words
        );
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "fake_ad").await
    }
}
