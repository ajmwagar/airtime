//! `caller` — GTA-mode call-in segment.
//!
//! The host introduces a fake caller; the LLM voices both sides. The full
//! script renders with the host's TTS voice for Phase 1 (a second Kokoro
//! instance for the caller voice is Phase 2).

use super::base::{
    recent_context_block, render_segment, system_prompt, Skill, SkillContext, SkillError,
    SkillOutput, SkillRuntime,
};
use async_trait::async_trait;

pub struct CallerSkill;

#[async_trait]
impl Skill for CallerSkill {
    fn name(&self) -> &'static str {
        "caller"
    }

    async fn generate(
        &self,
        ctx: &SkillContext,
        rt: &SkillRuntime,
    ) -> Result<SkillOutput, SkillError> {
        let cfg = ctx.persona.skill_config(self.name());
        let system = system_prompt(ctx);
        let user = format!(
            "Fake call-in. Caller name + one quirk, they say something brief and odd, \
             you react. Keep voices distinct. Max {} words.{}",
            cfg.max_words,
            recent_context_block(&ctx.recent),
        );
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "caller").await
    }
}
