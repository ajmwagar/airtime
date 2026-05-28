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
    /// How many tracks have played since the last break.
    tracks_since_break: usize,
    /// Insert a break every N tracks.
    tracks_per_break: usize,
    /// Name of the most recently chosen break skill — used as a tiebreak
    /// to encourage rotation among baseline candidates.
    last_break_name: Option<String>,
}

impl SegmentScheduler {
    pub fn new(skills: Vec<Arc<dyn Skill>>, tracks_per_break: usize) -> Self {
        Self {
            skills,
            tracks_since_break: 0,
            tracks_per_break: tracks_per_break.max(1),
            last_break_name: None,
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

    /// Pick the next non-track skill for the given minute of the hour.
    ///
    /// Selection rule: of the enabled non-track skills, drop any with a
    /// non-positive `time_score` (those are explicitly suppressed at
    /// this minute), then pick the highest-scoring. Ties are broken in
    /// favour of skills *different from the most recently chosen one*,
    /// which gives us natural rotation among baseline candidates without
    /// a separate round-robin index.
    pub fn next_break_skill_at(&mut self, minute: u32) -> Option<Arc<dyn Skill>> {
        let candidates: Vec<(i32, Arc<dyn Skill>)> = self
            .skills
            .iter()
            .filter(|s| !s.needs_track())
            .filter_map(|s| {
                let score = s.time_score(minute);
                if score > 0 {
                    Some((score, s.clone()))
                } else {
                    None
                }
            })
            .collect();

        if candidates.is_empty() {
            return None;
        }

        let max_score = candidates.iter().map(|(s, _)| *s).max().unwrap();
        let top_tier: Vec<Arc<dyn Skill>> = candidates
            .into_iter()
            .filter(|(s, _)| *s == max_score)
            .map(|(_, sk)| sk)
            .collect();

        // Prefer a skill different from the last one we ran. Fall back
        // to the first top-tier candidate if everything matches the
        // last-played name (e.g. only one candidate left).
        let pick = top_tier
            .iter()
            .find(|s| Some(s.name()) != self.last_break_name.as_deref())
            .cloned()
            .unwrap_or_else(|| top_tier[0].clone());

        self.last_break_name = Some(pick.name().to_string());
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

    /// Skill double with a custom `time_score` so we can drive the
    /// scoring logic deterministically.
    struct Scored {
        name: &'static str,
        score_at: fn(u32) -> i32,
    }

    #[async_trait]
    impl Skill for Scored {
        fn name(&self) -> &'static str {
            self.name
        }
        fn needs_track(&self) -> bool {
            false
        }
        fn time_score(&self, minute: u32) -> i32 {
            (self.score_at)(minute)
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
        let mut sched = SegmentScheduler::new(vec![Arc::new(Named("weather", false))], 2);
        sched.note_track_played();
        assert_eq!(sched.peek_next(), SlotKind::Track);
        sched.note_track_played();
        assert_eq!(sched.peek_next(), SlotKind::Break);
    }

    #[test]
    fn break_skills_rotate_among_baseline_ties() {
        // All three skills baseline-score 10 (the default), so the tiebreak
        // logic — "don't pick whatever you just picked" — does all the work.
        let mut sched = SegmentScheduler::new(
            vec![
                Arc::new(Named("weather", false)),
                Arc::new(Named("traffic", false)),
                Arc::new(Named("station_id", false)),
            ],
            2,
        );
        let first = sched.next_break_skill_at(8).unwrap().name();
        let second = sched.next_break_skill_at(8).unwrap().name();
        let third = sched.next_break_skill_at(8).unwrap().name();
        assert_ne!(first, second, "rotation should not repeat");
        assert_ne!(second, third, "rotation should not repeat");
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
        let pick = sched.next_break_skill_at(0).unwrap();
        assert_eq!(pick.name(), "weather");
    }

    #[test]
    fn time_aware_picks_top_of_hour_at_minute_zero() {
        let top = Arc::new(Scored {
            name: "top_of_hour",
            score_at: |m| if m < 5 { 100 } else { 0 },
        });
        let weather = Arc::new(Scored {
            name: "weather",
            score_at: |_| 10,
        });
        let mut sched: SegmentScheduler =
            SegmentScheduler::new(vec![top.clone(), weather.clone()], 2);
        assert_eq!(sched.next_break_skill_at(0).unwrap().name(), "top_of_hour");
        assert_eq!(sched.next_break_skill_at(3).unwrap().name(), "top_of_hour");
    }

    #[test]
    fn time_aware_suppresses_when_score_zero() {
        // `top_of_hour` returns 0 outside its window → eligible-only is `weather`.
        let top = Arc::new(Scored {
            name: "top_of_hour",
            score_at: |m| if m < 5 { 100 } else { 0 },
        });
        let weather = Arc::new(Scored {
            name: "weather",
            score_at: |_| 10,
        });
        let mut sched = SegmentScheduler::new(vec![top, weather], 2);
        assert_eq!(sched.next_break_skill_at(10).unwrap().name(), "weather");
    }

    #[test]
    fn time_aware_prefers_high_score_over_baseline() {
        let station = Arc::new(Scored {
            name: "station_id",
            score_at: |m| if (14..=16).contains(&m) { 50 } else { 10 },
        });
        let weather = Arc::new(Scored {
            name: "weather",
            score_at: |_| 10,
        });
        let mut sched = SegmentScheduler::new(vec![station, weather], 2);
        assert_eq!(sched.next_break_skill_at(15).unwrap().name(), "station_id");
        // Outside the preferred window the two tie at 10 — tiebreak picks
        // *some* eligible skill, but it should be one of them.
        let n = sched.next_break_skill_at(40).unwrap().name();
        assert!(n == "weather" || n == "station_id");
    }

    #[test]
    fn time_aware_returns_none_when_all_suppressed() {
        let s = Arc::new(Scored {
            name: "only_top",
            score_at: |m| if m < 5 { 100 } else { 0 },
        });
        let mut sched = SegmentScheduler::new(vec![s], 2);
        assert!(sched.next_break_skill_at(30).is_none());
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
