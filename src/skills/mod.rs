//! DJ segment skills.
//!
//! A `Skill` turns a `SkillContext` (persona + current track + feed
//! snapshots + station config) into a rendered audio segment. Skills are
//! pure orchestration: they ask the LLM for a script, hand it to the TTS,
//! then push the WAV through the audio processor to produce a
//! loudness-normalized FLAC.

pub mod base;
mod caller;
mod fake_ad;
mod station_id;
mod top_of_hour;
mod track_intro;
mod track_outro;
mod traffic;
mod weather;

pub use base::{
    Skill, SkillContext, SkillError, SkillOutput, SkillRuntime, SKILL_SCORE_BASELINE,
    SKILL_SCORE_PREFERRED, SKILL_SCORE_REQUIRED, SKILL_SCORE_SUPPRESSED,
};
pub use caller::CallerSkill;
pub use fake_ad::FakeAdSkill;
pub use station_id::StationIdSkill;
pub use top_of_hour::TopOfHourSkill;
pub use track_intro::TrackIntroSkill;
pub use track_outro::TrackOutroSkill;
pub use traffic::TrafficSkill;
pub use weather::WeatherSkill;

use crate::config::SkillToggles;
use std::sync::Arc;

/// Build the set of enabled skills for a persona. The order here is the
/// order skills appear in the toggle struct, which is also the order the
/// scheduler will iterate when picking a non-track segment.
pub fn build_enabled(toggles: &SkillToggles) -> Vec<Arc<dyn Skill>> {
    let mut out: Vec<Arc<dyn Skill>> = Vec::new();
    if toggles.track_intro {
        out.push(Arc::new(TrackIntroSkill));
    }
    if toggles.track_outro {
        out.push(Arc::new(TrackOutroSkill));
    }
    if toggles.top_of_hour {
        out.push(Arc::new(TopOfHourSkill));
    }
    if toggles.weather {
        out.push(Arc::new(WeatherSkill));
    }
    if toggles.traffic {
        out.push(Arc::new(TrafficSkill));
    }
    if toggles.station_id {
        out.push(Arc::new(StationIdSkill));
    }
    if toggles.fake_ad {
        out.push(Arc::new(FakeAdSkill));
    }
    if toggles.caller {
        out.push(Arc::new(CallerSkill));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SkillToggles;

    #[test]
    fn builds_only_enabled_skills() {
        let toggles = SkillToggles {
            track_intro: true,
            weather: true,
            station_id: true,
            ..Default::default()
        };
        let skills = build_enabled(&toggles);
        assert_eq!(skills.len(), 3);
        let names: Vec<_> = skills.iter().map(|s| s.name()).collect();
        assert!(names.contains(&"track_intro"));
        assert!(names.contains(&"weather"));
        assert!(names.contains(&"station_id"));
    }

    #[test]
    fn empty_toggles_yield_no_skills() {
        let skills = build_enabled(&SkillToggles::default());
        assert!(skills.is_empty());
    }

    /// Per-skill time-of-hour scoring. These lock in the slotting rules
    /// so a refactor can't silently slide `top_of_hour` away from the
    /// top of the hour, etc.
    #[test]
    fn top_of_hour_locked_to_first_five_minutes() {
        let s = TopOfHourSkill;
        assert!(s.time_score(0) > 0);
        assert!(s.time_score(4) > 0);
        assert_eq!(s.time_score(5), 0);
        assert_eq!(s.time_score(30), 0);
    }

    #[test]
    fn station_id_prefers_quarter_marks() {
        let s = StationIdSkill;
        let base = s.time_score(8);
        for m in [15, 30, 45] {
            assert!(
                s.time_score(m) > base,
                "minute {m} should outscore baseline"
            );
        }
    }

    #[test]
    fn weather_prefers_pre_news_and_post_news_windows() {
        let s = WeatherSkill;
        let base = s.time_score(10);
        for m in [25, 55] {
            assert!(s.time_score(m) > base, "minute {m} should beat baseline");
        }
    }

    #[test]
    fn traffic_prefers_weather_adjacent_slots() {
        let s = TrafficSkill;
        let base = s.time_score(10);
        for m in [22, 52] {
            assert!(s.time_score(m) > base, "minute {m} should beat baseline");
        }
    }

    #[test]
    fn baseline_skills_eligible_anywhere() {
        // fake_ad + caller stay at baseline — they're filler.
        for m in [0, 7, 15, 33, 59] {
            assert!(FakeAdSkill.time_score(m) > 0);
            assert!(CallerSkill.time_score(m) > 0);
        }
    }
}
