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

use crate::skills::{Skill, SKILL_SCORE_PREFERRED};
use std::collections::HashMap;
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
    /// Insert a break every N tracks. Can be updated mid-flight via
    /// `set_tracks_per_break` when the active daypart changes.
    tracks_per_break: usize,
    /// Name of the most recently chosen break skill — used as a tiebreak
    /// to encourage rotation among baseline candidates.
    last_break_name: Option<String>,
    /// For every `must_fire_per_hour` skill, the hour we last fired it
    /// in. Drives drift-correction: if the current hour differs from
    /// the recorded one and we're past the skill's preferred window,
    /// the skill becomes "overdue" and gets bumped to PREFERRED so it
    /// wins the next break slot.
    last_fired_hour: HashMap<&'static str, u32>,
    /// Skill name → hour-of-day spec (`"06-10,15-19"`). When the
    /// current hour matches, the skill gets a PREFERRED-score bump
    /// regardless of minute — used for tying skills to commute hours,
    /// late-night blocks, etc. Populated from per-skill
    /// `[host.skill_config.X].preferred_hours` in the persona TOML.
    preferred_hours: HashMap<&'static str, String>,
}

impl SegmentScheduler {
    pub fn new(skills: Vec<Arc<dyn Skill>>, tracks_per_break: usize) -> Self {
        Self {
            skills,
            tracks_since_break: 0,
            tracks_per_break: tracks_per_break.max(1),
            last_break_name: None,
            last_fired_hour: HashMap::new(),
            preferred_hours: HashMap::new(),
        }
    }

    /// Update the music cadence — called by the producer at the start
    /// of each scheduling decision so daypart changes take effect
    /// immediately rather than at the next hour boundary.
    pub fn set_tracks_per_break(&mut self, n: usize) {
        self.tracks_per_break = n.max(1);
    }

    /// Install the per-skill-name → hours-of-day map. Producer builds
    /// this once at startup from the persona's skill_config. When the
    /// current hour falls inside a skill's spec, that skill gets a
    /// PREFERRED-score bump regardless of minute.
    pub fn set_preferred_hours(&mut self, map: HashMap<&'static str, String>) {
        self.preferred_hours = map;
    }

    /// Returns the kind of segment the scheduler wants next.
    ///
    /// Drift correction: if any `must_fire_per_hour` skill is overdue
    /// (we're past its preferred window and it hasn't fired this hour),
    /// force a `Break` slot regardless of `tracks_since_break`.
    /// Otherwise: `Break` once the music-cadence quota is reached.
    pub fn peek_next(&self, hour: u32, minute: u32) -> SlotKind {
        if self.has_overdue_anchor(hour, minute) {
            return SlotKind::Break;
        }
        if self.tracks_since_break >= self.tracks_per_break {
            SlotKind::Break
        } else {
            SlotKind::Track
        }
    }

    /// Is any hour-anchor skill overdue? "Overdue" = `must_fire_per_hour`
    /// AND we haven't fired it this hour AND its `time_score(minute)` is
    /// now zero (we're past its preferred slot).
    fn has_overdue_anchor(&self, hour: u32, minute: u32) -> bool {
        self.skills.iter().any(|s| {
            s.must_fire_per_hour()
                && self.last_fired_hour.get(s.name()) != Some(&hour)
                && s.time_score(minute) == 0
        })
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

    /// Mark a break slot as resolved without a skill having fired —
    /// nothing was eligible. Resets the cadence so the next slot is a
    /// Track again; without this, `peek_next` stays pegged at `Break`
    /// and the producer loop spins (no send → no yield → starves the
    /// receiver task).
    pub fn note_break_skipped(&mut self) {
        self.tracks_since_break = 0;
    }

    /// Pick the next non-track skill for the given hour + minute.
    ///
    /// Selection rule: score each enabled non-track skill, drop any
    /// with a non-positive score, pick the highest. Two extras on top:
    ///
    /// **Drift correction for hour-anchor skills.** If a skill is
    /// `must_fire_per_hour` and hasn't fired this hour, and its
    /// `time_score(minute)` returns 0 (past its preferred window), we
    /// override the score to `PREFERRED` so the skill competes for
    /// makeup priority — instead of getting silently skipped because a
    /// long track straddled its slot.
    ///
    /// **Tiebreak.** Among top-scoring candidates, prefer one different
    /// from the most recently played skill — natural rotation.
    pub fn next_break_skill_at(&mut self, hour: u32, minute: u32) -> Option<Arc<dyn Skill>> {
        let candidates: Vec<(i32, Arc<dyn Skill>)> = self
            .skills
            .iter()
            .filter(|s| !s.needs_track())
            .filter_map(|s| {
                let mut score = s.time_score(minute);
                let in_preferred_hour = self
                    .preferred_hours
                    .get(s.name())
                    .map(|spec| crate::config::hours_contain(spec, hour))
                    .unwrap_or(false);
                if in_preferred_hour && score < SKILL_SCORE_PREFERRED {
                    score = SKILL_SCORE_PREFERRED;
                }
                let needs_makeup =
                    s.must_fire_per_hour() && self.last_fired_hour.get(s.name()) != Some(&hour);
                if needs_makeup && score < SKILL_SCORE_PREFERRED {
                    score = SKILL_SCORE_PREFERRED;
                }
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

        let pick = top_tier
            .iter()
            .find(|s| Some(s.name()) != self.last_break_name.as_deref())
            .cloned()
            .unwrap_or_else(|| top_tier[0].clone());

        self.last_break_name = Some(pick.name().to_string());
        self.tracks_since_break = 0;
        // Mark hour-anchor skills as "fired this hour" so drift logic
        // won't keep firing them every break for the rest of the hour.
        if pick.must_fire_per_hour() {
            self.last_fired_hour.insert(pick.name(), hour);
        }
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
        assert_eq!(sched.peek_next(12, 0), SlotKind::Track);
    }

    #[test]
    fn switches_to_break_after_quota() {
        let mut sched = SegmentScheduler::new(vec![Arc::new(Named("weather", false))], 2);
        sched.note_track_played();
        assert_eq!(sched.peek_next(12, 0), SlotKind::Track);
        sched.note_track_played();
        assert_eq!(sched.peek_next(12, 0), SlotKind::Break);
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
        let first = sched.next_break_skill_at(12, 8).unwrap().name();
        let second = sched.next_break_skill_at(12, 8).unwrap().name();
        let third = sched.next_break_skill_at(12, 8).unwrap().name();
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
        let pick = sched.next_break_skill_at(12, 0).unwrap();
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
        assert_eq!(
            sched.next_break_skill_at(12, 0).unwrap().name(),
            "top_of_hour"
        );
        assert_eq!(
            sched.next_break_skill_at(12, 3).unwrap().name(),
            "top_of_hour"
        );
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
        assert_eq!(sched.next_break_skill_at(12, 10).unwrap().name(), "weather");
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
        assert_eq!(
            sched.next_break_skill_at(12, 15).unwrap().name(),
            "station_id"
        );
        // Outside the preferred window the two tie at 10 — tiebreak picks
        // *some* eligible skill, but it should be one of them.
        let n = sched.next_break_skill_at(12, 40).unwrap().name();
        assert!(n == "weather" || n == "station_id");
    }

    #[test]
    fn time_aware_returns_none_when_all_suppressed() {
        let s = Arc::new(Scored {
            name: "only_top",
            score_at: |m| if m < 5 { 100 } else { 0 },
        });
        let mut sched = SegmentScheduler::new(vec![s], 2);
        assert!(sched.next_break_skill_at(12, 30).is_none());
    }

    /// `Scored` with an `must_fire_per_hour` knob for drift testing.
    struct Anchor {
        name: &'static str,
        score_at: fn(u32) -> i32,
        anchor: bool,
    }

    #[async_trait]
    impl Skill for Anchor {
        fn name(&self) -> &'static str {
            self.name
        }
        fn needs_track(&self) -> bool {
            false
        }
        fn time_score(&self, minute: u32) -> i32 {
            (self.score_at)(minute)
        }
        fn must_fire_per_hour(&self) -> bool {
            self.anchor
        }
        async fn generate(
            &self,
            _ctx: &SkillContext,
            _rt: &SkillRuntime,
        ) -> Result<SkillOutput, SkillError> {
            unimplemented!()
        }
    }

    /// Drift correction: top_of_hour-style skill that *should* fire at
    /// :00–:04 but the producer's mid-track at :03. At :08 a long song
    /// finally ends. Scheduler should: (1) recognise overdue, force
    /// Break slot. (2) bump top_of_hour to PREFERRED so it wins the slot
    /// against a baseline weather skill.
    #[test]
    fn drift_correction_fires_overdue_anchor_skill() {
        let top = Arc::new(Anchor {
            name: "top_of_hour",
            score_at: |m| if m < 5 { 100 } else { 0 },
            anchor: true,
        });
        let weather = Arc::new(Anchor {
            name: "weather",
            score_at: |_| 10,
            anchor: false,
        });
        let mut sched = SegmentScheduler::new(vec![top, weather], 3);
        // Not in preferred window (minute 8), top_of_hour hasn't fired this hour:
        // overdue logic kicks in.
        assert_eq!(sched.peek_next(14, 8), SlotKind::Break);
        let pick = sched.next_break_skill_at(14, 8).unwrap();
        assert_eq!(pick.name(), "top_of_hour");
    }

    /// Once an anchor skill has fired in a given hour, drift-correction
    /// is silent for that hour even if `time_score` would otherwise
    /// signal "overdue" — no double-news in the same hour.
    #[test]
    fn drift_correction_silent_after_anchor_fires_this_hour() {
        let top = Arc::new(Anchor {
            name: "top_of_hour",
            score_at: |m| if m < 5 { 100 } else { 0 },
            anchor: true,
        });
        let weather = Arc::new(Anchor {
            name: "weather",
            score_at: |_| 10,
            anchor: false,
        });
        let mut sched = SegmentScheduler::new(vec![top, weather], 3);
        // Fire top_of_hour in its preferred window.
        let pick = sched.next_break_skill_at(14, 2).unwrap();
        assert_eq!(pick.name(), "top_of_hour");
        // Now mid-hour, no overdue logic should fire.
        assert_eq!(sched.peek_next(14, 20), SlotKind::Track);
        // And asking for the next break in this hour picks weather.
        let pick2 = sched.next_break_skill_at(14, 30).unwrap();
        assert_eq!(pick2.name(), "weather");
    }

    /// Drift correction resets each hour: even after firing at hour 14,
    /// the same anchor skill is overdue again at hour 15 minute 8.
    #[test]
    fn drift_correction_resets_on_new_hour() {
        let top = Arc::new(Anchor {
            name: "top_of_hour",
            score_at: |m| if m < 5 { 100 } else { 0 },
            anchor: true,
        });
        let weather = Arc::new(Anchor {
            name: "weather",
            score_at: |_| 10,
            anchor: false,
        });
        let mut sched = SegmentScheduler::new(vec![top, weather], 3);
        sched.next_break_skill_at(14, 2);
        // Skip ahead to hour 15 — top_of_hour overdue again.
        assert_eq!(sched.peek_next(15, 8), SlotKind::Break);
        let pick = sched.next_break_skill_at(15, 8).unwrap();
        assert_eq!(pick.name(), "top_of_hour");
    }

    /// Daypart cadence change: scheduler honours `set_tracks_per_break`
    /// immediately so the next slot decision uses the new cadence.
    #[test]
    fn set_tracks_per_break_updates_cadence_immediately() {
        let mut sched = SegmentScheduler::new(vec![Arc::new(Named("weather", false))], 5);
        for _ in 0..3 {
            sched.note_track_played();
        }
        // Still 3 < 5, so Track.
        assert_eq!(sched.peek_next(12, 0), SlotKind::Track);
        // Switch to a tighter cadence — 3 tracks already played meets quota 2.
        sched.set_tracks_per_break(2);
        assert_eq!(sched.peek_next(12, 0), SlotKind::Break);
    }

    /// `set_preferred_hours` boosts a skill to PREFERRED regardless of
    /// minute when the current hour is in its spec — drives commute-hour
    /// traffic reads, late-night fake-ads, etc.
    #[test]
    fn preferred_hours_boost_wins_against_baseline_competitor() {
        // Two skills, both at baseline at this minute. Traffic gets a
        // commute-hour boost; weather doesn't — traffic must win.
        let traffic = Arc::new(Named("traffic", false));
        let weather = Arc::new(Named("weather", false));
        let mut sched = SegmentScheduler::new(vec![traffic, weather], 2);
        let mut hours = HashMap::new();
        hours.insert("traffic", "06-10".to_string());
        sched.set_preferred_hours(hours);
        sched.note_track_played();
        sched.note_track_played();
        // Hour 8 is in commute window; minute 33 is no skill's preferred
        // window so without the boost, both would tie at baseline.
        let pick = sched.next_break_skill_at(8, 33).unwrap();
        assert_eq!(pick.name(), "traffic");
    }

    #[test]
    fn preferred_hours_outside_spec_does_nothing() {
        // Traffic at baseline + a Scored competitor pinned to PREFERRED.
        // Inside commute → traffic ties → could win on rotation. Outside
        // commute → competitor outranks → competitor wins outright.
        let traffic = Arc::new(Named("traffic", false));
        let competitor = Arc::new(Scored {
            name: "station_id",
            score_at: |_| SKILL_SCORE_PREFERRED,
        });
        let mut sched = SegmentScheduler::new(vec![traffic, competitor], 2);
        let mut hours = HashMap::new();
        hours.insert("traffic", "06-10".to_string());
        sched.set_preferred_hours(hours);
        sched.note_track_played();
        sched.note_track_played();
        let pick = sched.next_break_skill_at(14, 30).unwrap();
        assert_eq!(
            pick.name(),
            "station_id",
            "outside commute hours, traffic shouldn't get the boost"
        );
    }

    /// `note_break_skipped` resets the cadence so a no-eligible-skill
    /// break doesn't leave `peek_next` pegged at `Break`. Without this,
    /// the producer loop spins without sending or yielding.
    #[test]
    fn note_break_skipped_resets_cadence() {
        let mut sched = SegmentScheduler::new(vec![], 2);
        sched.note_track_played();
        sched.note_track_played();
        assert_eq!(sched.peek_next(12, 0), SlotKind::Break);
        sched.note_break_skipped();
        assert_eq!(sched.peek_next(12, 0), SlotKind::Track);
    }

    /// Cadence floor: `set_tracks_per_break(0)` is clamped to 1, so the
    /// scheduler doesn't deadlock on a "break after zero tracks" config.
    #[test]
    fn set_tracks_per_break_clamps_zero_to_one() {
        let mut sched = SegmentScheduler::new(vec![Arc::new(Named("weather", false))], 3);
        sched.set_tracks_per_break(0);
        // After 1 track played, peek_next should return Break (clamped to 1).
        sched.note_track_played();
        assert_eq!(sched.peek_next(12, 0), SlotKind::Break);
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
