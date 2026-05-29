//! `fake_ad` — GTA-mode parody ad spot for a fictional product.

use super::base::{
    recent_context_block, render_segment, system_prompt, Skill, SkillContext, SkillError,
    SkillOutput, SkillRuntime,
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
        let system = system_prompt(ctx);
        let user = format!(
            "Parody radio ad. Fictional product, era-appropriate, over-the-top. \
             Snap the hook in the first sentence. End with tagline or fake phone number. \
             Max {} words.{}",
            cfg.max_words,
            recent_context_block(&ctx.recent),
        );
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "fake_ad").await
    }
}
