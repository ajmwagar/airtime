//! Segment scheduler.
//!
//! Picks the next skill to run based on:
//! - elapsed slot count within the hour (rotates non-track skills)
//! - the current track context (track_intro/outro only run when a track
//!   is queued)
//!
//! Phase 1 is intentionally simple: cycle the enabled non-track skills,
//! interleave with track_intro / track_outro around each music track. A
//! richer day-parted scheduler can replace this without touching the
//! `Skill` interface.

use crate::skills::Skill;
use std::sync::Arc;

/// (intro, outro) skills for a music slot.
pub type TrackSkills = (Option<Arc<dyn Skill>>, Option<Arc<dyn Skill>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// Music track plus its intro/outro chrome.
    Track,
    /// Non-track segment (weather, news, ID, …).
    Break,
}

pub struct SegmentScheduler {
    skills: Vec<Arc<dyn Skill>>,
    non_track_idx: usize,
    /// How many tracks have played since the last break.
    tracks_since_break: usize,
    /// Insert a break every N tracks.
    tracks_per_break: usize,
}

impl SegmentScheduler {
    pub fn new(skills: Vec<Arc<dyn Skill>>, tracks_per_break: usize) -> Self {
        Self {
            skills,
            non_track_idx: 0,
            tracks_since_break: 0,
            tracks_per_break: tracks_per_break.max(1),
        }
    }

    /// Returns the kind of segment the scheduler wants next. The caller
    /// then either plays a music track (and optionally bracket it with
    /// `next_track_skills`) or asks for `next_break_skill`.
    pub fn peek_next(&self) -> SlotKind {
        if self.tracks_since_break >= self.tracks_per_break {
            SlotKind::Break
        } else {
            SlotKind::Track
        }
    }

    /// Skills to run alongside a music track: `track_intro` (if enabled)
    /// before the track, `track_outro` (if enabled) after it. Returned as
    /// `(intro, outro)`.
    pub fn track_skills(&self) -> TrackSkills {
        let intro = self
            .skills
            .iter()
            .find(|s| s.name() == "track_intro")
            .cloned();
        let outro = self
            .skills
            .iter()
            .find(|s| s.name() == "track_outro")
            .cloned();
        (intro, outro)
    }

    /// Advance the scheduler after a track has been played.
    pub fn note_track_played(&mut self) {
        self.tracks_since_break += 1;
    }

    /// Pick the next non-track skill. Round-robins through whatever's
    /// enabled (excluding track_intro/outro since those are slotted
    /// against music).
    pub fn next_break_skill(&mut self) -> Option<Arc<dyn Skill>> {
        let candidates: Vec<_> = self
            .skills
            .iter()
            .filter(|s| !s.needs_track())
            .cloned()
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let pick = candidates[self.non_track_idx % candidates.len()].clone();
        self.non_track_idx = self.non_track_idx.wrapping_add(1);
        self.tracks_since_break = 0;
        Some(pick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::base::{SkillContext, SkillError, SkillOutput, SkillRuntime};
    use async_trait::async_trait;

    struct Named(&'static str, bool);

    #[async_trait]
    impl Skill for Named {
        fn name(&self) -> &'static str {
            self.0
        }
        fn needs_track(&self) -> bool {
            self.1
        }
        async fn generate(
            &self,
            _ctx: &SkillContext,
            _rt: &SkillRuntime,
        ) -> Result<SkillOutput, SkillError> {
            unimplemented!()
        }
    }

    #[test]
    fn peeks_track_when_under_quota() {
        let sched = SegmentScheduler::new(
            vec![
                Arc::new(Named("track_intro", true)),
                Arc::new(Named("weather", false)),
            ],
            3,
        );
        assert_eq!(sched.peek_next(), SlotKind::Track);
    }

    #[test]
    fn switches_to_break_after_quota() {
        let mut sched = SegmentScheduler::new(
            vec![Arc::new(Named("weather", false))],
            2,
        );
        sched.note_track_played();
        assert_eq!(sched.peek_next(), SlotKind::Track);
        sched.note_track_played();
        assert_eq!(sched.peek_next(), SlotKind::Break);
    }

    #[test]
    fn break_skills_round_robin() {
        let mut sched = SegmentScheduler::new(
            vec![
                Arc::new(Named("weather", false)),
                Arc::new(Named("traffic", false)),
                Arc::new(Named("station_id", false)),
            ],
            2,
        );
        assert_eq!(sched.next_break_skill().unwrap().name(), "weather");
        assert_eq!(sched.next_break_skill().unwrap().name(), "traffic");
        assert_eq!(sched.next_break_skill().unwrap().name(), "station_id");
        assert_eq!(sched.next_break_skill().unwrap().name(), "weather");
    }

    #[test]
    fn breaks_skip_track_only_skills() {
        let mut sched = SegmentScheduler::new(
            vec![
                Arc::new(Named("track_intro", true)),
                Arc::new(Named("weather", false)),
            ],
            1,
        );
        let pick = sched.next_break_skill().unwrap();
        assert_eq!(pick.name(), "weather");
    }

    #[test]
    fn track_skills_returns_intro_and_outro() {
        let sched = SegmentScheduler::new(
            vec![
                Arc::new(Named("track_intro", true)),
                Arc::new(Named("track_outro", true)),
                Arc::new(Named("weather", false)),
            ],
            2,
        );
        let (intro, outro) = sched.track_skills();
        assert!(intro.is_some());
        assert!(outro.is_some());
        assert_eq!(intro.unwrap().name(), "track_intro");
        assert_eq!(outro.unwrap().name(), "track_outro");
    }
}
