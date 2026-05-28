//! EQ profile presets.
//!
//! Each profile is just a string of FFmpeg `-af` filter chain syntax. We
//! keep them as data so the audio pipeline can pick by name. New
//! profiles can be added without touching code paths elsewhere.

/// Returns the FFmpeg filter chain for a named profile, or `None` if the
/// profile is unknown (caller decides whether that's fatal).
pub fn profile_filter(name: &str) -> Option<&'static str> {
    match name {
        "warm_analog" => Some(
            // Gentle low-shelf bump, soft top-end roll-off, slight saturation.
            "equalizer=f=120:t=h:width=200:g=2,equalizer=f=8000:t=h:width=2000:g=-3,acompressor=ratio=2:attack=20:release=250",
        ),
        "broadcast_crunch" => Some(
            // AM-radio-ish: narrow-band telephone-y, lots of mids, heavy comp.
            // threshold=0.1 is approximately -20dB in linear scale
            "highpass=f=200,lowpass=f=6000,acompressor=ratio=6:attack=5:release=80:threshold=0.1",
        ),
        "telephone" => Some(
            // 8 kHz μ-law-ish band, narrow voice cut.
            "highpass=f=300,lowpass=f=3400,acompressor=ratio=4:attack=5:release=60",
        ),
        _ => None,
    }
}

pub fn known_profiles() -> &'static [&'static str] {
    &["warm_analog", "broadcast_crunch", "telephone"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_known_profiles_resolve() {
        for p in known_profiles() {
            assert!(profile_filter(p).is_some(), "{p} should resolve");
        }
    }

    #[test]
    fn unknown_profile_returns_none() {
        assert!(profile_filter("disco_destroyer").is_none());
    }
}
