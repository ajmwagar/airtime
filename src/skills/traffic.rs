//! `traffic` — local incident summary read.

use super::base::{
    recent_context_block, render_segment, system_prompt, Skill, SkillContext, SkillError,
    SkillOutput, SkillRuntime, SKILL_SCORE_BASELINE, SKILL_SCORE_PREFERRED,
};
use async_trait::async_trait;

pub struct TrafficSkill;

#[async_trait]
impl Skill for TrafficSkill {
    fn name(&self) -> &'static str {
        "traffic"
    }

    /// Traffic reads pair with the weather windows on radio — :22ish and
    /// :52ish are the canonical "weather and traffic" slots.
    fn time_score(&self, minute: u32) -> i32 {
        match minute {
            21..=24 | 51..=54 => SKILL_SCORE_PREFERRED,
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
        let user = match ctx.feeds.traffic.as_ref() {
            // Four branches by severity. The previous single branch had
            // the LLM parroting the raw incident count ("618 incidents
            // out there") because all it got was the number. Now we
            // pick the right framing per state and hand it specific
            // slowdowns to name.
            Some(t) if t.incidents == 0 => format!(
                "Roads are clear right now. One brief, upbeat mention \
                 then back to music. Max {max} words.{recent}",
                max = cfg.max_words,
                recent = recent_context_block(&ctx.recent),
            ),
            Some(t) if t.significant == 0 => format!(
                "Roads are mostly clear — only minor things on the map. \
                 Quick reassuring line, don't dwell. Max {max} words.{recent}",
                max = cfg.max_words,
                recent = recent_context_block(&ctx.recent),
            ),
            Some(t) => {
                let worst_lines = t
                    .worst
                    .iter()
                    .map(|i| format!("- {}", i.one_line()))
                    .collect::<Vec<_>>()
                    .join("\n");
                let total_minutes = t.total_delay_seconds / 60;
                format!(
                    "Traffic update. {sig} active slowdowns across the metro, about \
                     {mins} minutes of cumulative delay. The worst right now:\n{worst}\n\n\
                     Mention one or two specific slowdowns by road name and rough delay. \
                     Don't recite the whole list, don't say the raw incident count. \
                     Close with a driving tip if there's room. Max {max} words.{recent}",
                    sig = t.significant,
                    mins = total_minutes,
                    worst = worst_lines,
                    max = cfg.max_words,
                    recent = recent_context_block(&ctx.recent),
                )
            }
            None => format!(
                "Traffic data's out right now — one honest line saying so, \
                 then move on. Don't make up incidents. Max {max} words.{recent}",
                max = cfg.max_words,
                recent = recent_context_block(&ctx.recent),
            ),
        };
        let backend = ctx.persona.resolve_llm_backend(self.name());
        let script = rt.llm.complete(&system, &user, &backend).await?;
        render_segment(rt, &ctx.persona, script, "traffic").await
    }
}
