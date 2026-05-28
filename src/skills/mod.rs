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

pub use base::{Skill, SkillContext, SkillError, SkillOutput, SkillRuntime};
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
}
