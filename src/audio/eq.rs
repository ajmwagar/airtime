//! EQ profile presets.
//!
//! Each profile is just a string of FFmpeg `-af` filter chain syntax. We
//! keep them as data so the audio pipeline can pick by name. New
//! profiles can be added without touching code paths elsewhere.
//!
//! **Threshold gotcha:** FFmpeg's `acompressor` `threshold` parameter is
//! a *linear amplitude* value in `[0.000977..1.0]`, **not** dB. Roughly:
//! `-20 dB ≈ 0.1`, `-12 dB ≈ 0.25`, `-6 dB ≈ 0.5`. Passing a dB value
//! makes ffmpeg blow up at filter-init time with
//! `"Value … for parameter 'threshold' out of range"` and aborts the
//! whole encode — the test below catches profiles that drift past the
//! valid linear range.

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
            // threshold=0.1 ≈ -20 dB linear (acompressor wants linear, not dB).
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

    /// Regression test for the `acompressor:threshold=-20` bug — any
    /// `threshold=…` token in a built-in profile must be a linear
    /// amplitude in FFmpeg's documented range `[0.000976563..1.0]`. A
    /// dB-shaped value (negative, or |x| > 1) makes ffmpeg refuse the
    /// filter graph at runtime and abort the encode.
    #[test]
    fn acompressor_thresholds_are_linear_amplitude() {
        const MIN_THRESHOLD: f64 = 0.000_976_563;
        const MAX_THRESHOLD: f64 = 1.0;

        for name in known_profiles() {
            let chain = profile_filter(name).unwrap();
            for token in chain.split([',', ':']) {
                let Some(rest) = token.strip_prefix("threshold=") else {
                    continue;
                };
                let value: f64 = rest.parse().unwrap_or_else(|_| {
                    panic!("profile {name}: threshold token {token:?} did not parse as f64")
                });
                assert!(
                    (MIN_THRESHOLD..=MAX_THRESHOLD).contains(&value),
                    "profile {name}: threshold={value} is outside FFmpeg's linear range \
                     [{MIN_THRESHOLD}..={MAX_THRESHOLD}] — looks like a dB value, \
                     convert via 10^(dB/20) (-20 dB ≈ 0.1, -12 dB ≈ 0.25, -6 dB ≈ 0.5)"
                );
            }
        }
    }
}
