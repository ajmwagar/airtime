//! `caller` — GTA-mode call-in segment.
//!
//! The host introduces a fake caller; the LLM voices both sides. The full
//! script renders with the host's TTS voice for Phase 1 (a second Kokoro
//! instance for the caller voice is Phase 2).

use super::base::{render_segment, system_prompt, Skill, SkillContext, SkillError, SkillOutput, SkillRuntime};
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
        let system = system_prompt(&ctx.persona);
        let user = format!(
            "Write a short fake call-in segment. Introduce the caller (give them a name and \
             vibe), have them say something brief and odd, then react in your voice. Keep both \
             sides distinct but believable. Max {} words.",
            cfg.max_words
        );
        let script = rt.llm.complete(&system, &user, &cfg.llm_backend).await?;
        render_segment(rt, &ctx.persona, script, "caller").await
    }
}
