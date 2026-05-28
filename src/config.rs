//! Settings (`settings.toml`) and persona (`personas/*.toml`) loaders.
//!
//! Personas are the per-station configs: host identity, enabled skills,
//! and per-skill overrides. Settings are the global, infrastructure-level
//! config: Icecast endpoint, model paths, feed credentials, audio defaults.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Global, infrastructure-level configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub icecast: IcecastSettings,
    pub ollama: OllamaSettings,
    pub claude: ClaudeSettings,
    #[serde(default)]
    pub openrouter: Option<OpenRouterSettings>,
    pub kokoro: KokoroSettings,
    pub library: LibrarySettings,
    pub feeds: FeedsSettings,
    pub audio: AudioSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IcecastSettings {
    pub host: String,
    pub port: u16,
    pub password: String,
    #[serde(default = "default_icecast_user")]
    pub user: String,
}

fn default_icecast_user() -> String {
    "source".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct OllamaSettings {
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeSettings {
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenRouterSettings {
    /// Model identifier, e.g. `anthropic/claude-3.5-sonnet`,
    /// `meta-llama/llama-3.1-70b-instruct`,
    /// `google/gemini-2.0-flash-exp:free`. See
    /// <https://openrouter.ai/models> for the live catalogue.
    pub model: String,
    /// Override the default `https://openrouter.ai/api/v1` endpoint
    /// (rarely useful outside of tests).
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KokoroSettings {
    pub model_path: PathBuf,
    pub voices_path: PathBuf,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default)]
    pub binary: Option<PathBuf>,
}

fn default_sample_rate() -> u32 {
    24_000
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibrarySettings {
    pub path: PathBuf,
    pub formats: Vec<String>,
    #[serde(default = "yes")]
    pub scan_on_start: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeedsSettings {
    pub weather_lat: f64,
    pub weather_lon: f64,
    #[serde(default)]
    pub traffic: Option<TrafficSettings>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrafficSettings {
    pub provider: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AudioSettings {
    pub temp_dir: PathBuf,
    pub loudness_target: f64,
    pub true_peak: f64,
}

impl Settings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.into(),
            source: e,
        })?;
        toml::from_str(&raw).map_err(|e| ConfigError::Parse {
            path: path.into(),
            source: e,
        })
    }
}

/// A single station's host configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Persona {
    pub host: Host,
    pub stream: Stream,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Host {
    pub name: String,
    pub callsign: String,
    pub era: String,
    pub genre: Vec<String>,
    pub voice_model: String,
    pub tone_prompt: String,
    pub skills: SkillToggles,
    #[serde(default)]
    pub skill_config: HashMap<String, SkillConfig>,
    pub audio: HostAudio,
    /// Persona-wide default LLM backend. Skills inherit this when their
    /// own `[host.skill_config.X].llm_backend` is unset. If this is also
    /// unset, the ultimate fallback is `"ollama"`.
    #[serde(default)]
    pub default_llm_backend: Option<String>,
    /// Music tracks between non-track segments (default cadence). A
    /// daypart can override this — see `[[host.dayparts]]`.
    #[serde(default = "default_tracks_per_break")]
    pub tracks_per_break: usize,
    /// `"imperial"` or `"metric"` — controls every measurement the host
    /// speaks aloud (currently temperature + wind, future: distance,
    /// pressure). Defaults to imperial because most US-based stations
    /// want that; flip to `"metric"` for everywhere else. Always
    /// spelled out by the skills (`"degrees Fahrenheit"`, `"miles per
    /// hour"`) — Kokoro pronounces abbreviations badly.
    #[serde(default = "default_units")]
    pub units: String,
    /// Day-part programming — different tracks-per-break cadences and
    /// tonal overlays depending on the hour. First matching daypart
    /// wins (so overlap is operator's problem). Empty means "same
    /// programming all day."
    #[serde(default)]
    pub dayparts: Vec<Daypart>,
}

fn default_tracks_per_break() -> usize {
    3
}

fn default_units() -> String {
    "imperial".into()
}

impl Host {
    /// `true` if the persona has selected imperial units.
    pub fn imperial(&self) -> bool {
        self.units.eq_ignore_ascii_case("imperial")
    }
}

/// A time-of-day programming block. `hours` is a comma-separated list
/// of `HH-HH` ranges, end-exclusive (`"06-09"` covers 6, 7, 8). Ranges
/// can wrap midnight: `"22-04"` covers 22, 23, 0, 1, 2, 3.
#[derive(Debug, Clone, Deserialize)]
pub struct Daypart {
    pub name: String,
    pub hours: String,
    /// Override `host.tracks_per_break` during this daypart.
    #[serde(default)]
    pub tracks_per_break: Option<usize>,
    /// Tone overlay appended to the system prompt — gives the host a
    /// distinct vibe per daypart without rewriting their persona.
    #[serde(default)]
    pub extra_tone: Option<String>,
}

/// Check whether `hour` (0..24) falls inside `spec`'s ranges.
///
/// `spec` syntax: comma-separated `HH-HH` half-open ranges; end may be
/// smaller than start to indicate a midnight wrap. Malformed entries
/// are silently ignored — operator should see them at config-parse
/// time once we surface a validation pass.
pub fn hours_contain(spec: &str, hour: u32) -> bool {
    for range in spec.split(',') {
        let range = range.trim();
        let Some((s, e)) = range.split_once('-') else {
            continue;
        };
        let Ok(start) = s.trim().parse::<u32>() else {
            continue;
        };
        let Ok(end) = e.trim().parse::<u32>() else {
            continue;
        };
        let hit = if start < end {
            hour >= start && hour < end
        } else if start > end {
            // wraps midnight
            hour >= start || hour < end
        } else {
            // start == end → empty / full-day depending on convention.
            // Treat as empty (operator can write "00-24" for full day).
            false
        };
        if hit {
            return true;
        }
    }
    false
}

impl Host {
    /// Find the current daypart for `hour` (0..24). First match wins,
    /// per declaration order in the TOML.
    pub fn current_daypart(&self, hour: u32) -> Option<&Daypart> {
        self.dayparts.iter().find(|d| hours_contain(&d.hours, hour))
    }

    /// `tracks_per_break` taking the active daypart into account.
    pub fn tracks_per_break_at(&self, hour: u32) -> usize {
        self.current_daypart(hour)
            .and_then(|d| d.tracks_per_break)
            .unwrap_or(self.tracks_per_break)
    }
}

/// Celsius → Fahrenheit. The weather feed returns C; we present in
/// whichever unit the persona asked for.
pub fn celsius_to_fahrenheit(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// Kilometres-per-hour → miles-per-hour. Open-Meteo's `windspeed` is
/// km/h; persona-side we convert when imperial.
pub fn kph_to_mph(kph: f64) -> f64 {
    kph * 0.621_371
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SkillToggles {
    #[serde(default)]
    pub track_intro: bool,
    #[serde(default)]
    pub track_outro: bool,
    #[serde(default)]
    pub top_of_hour: bool,
    #[serde(default)]
    pub weather: bool,
    #[serde(default)]
    pub traffic: bool,
    #[serde(default)]
    pub station_id: bool,
    #[serde(default)]
    pub fake_ad: bool,
    #[serde(default)]
    pub caller: bool,
}

impl SkillToggles {
    pub fn enabled(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.track_intro {
            out.push("track_intro");
        }
        if self.track_outro {
            out.push("track_outro");
        }
        if self.top_of_hour {
            out.push("top_of_hour");
        }
        if self.weather {
            out.push("weather");
        }
        if self.traffic {
            out.push("traffic");
        }
        if self.station_id {
            out.push("station_id");
        }
        if self.fake_ad {
            out.push("fake_ad");
        }
        if self.caller {
            out.push("caller");
        }
        out
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillConfig {
    #[serde(default = "default_max_words")]
    pub max_words: usize,
    /// Per-skill backend override. `None` means "use the persona's
    /// `default_llm_backend`, falling back to ollama if that's also
    /// unset" — see `Persona::resolve_llm_backend`.
    #[serde(default)]
    pub llm_backend: Option<String>,
    #[serde(default)]
    pub news_sources: Vec<String>,
}

impl Default for SkillConfig {
    fn default() -> Self {
        Self {
            max_words: default_max_words(),
            llm_backend: None,
            news_sources: Vec::new(),
        }
    }
}

fn default_max_words() -> usize {
    40
}

/// Ultimate fallback backend when neither the per-skill nor the
/// persona-wide default is set. Free + local, so airtime keeps working
/// without API keys.
pub const FALLBACK_LLM_BACKEND: &str = "ollama";

#[derive(Debug, Clone, Deserialize)]
pub struct HostAudio {
    #[serde(default)]
    pub eq_profile: Option<String>,
    #[serde(default)]
    pub room_tone: bool,
    pub loudness_target: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Stream {
    pub mount: String,
    pub format: String,
    #[serde(default)]
    pub bitrate: Option<u32>,
}

impl Persona {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.into(),
            source: e,
        })?;
        toml::from_str(&raw).map_err(|e| ConfigError::Parse {
            path: path.into(),
            source: e,
        })
    }

    pub fn skill_config(&self, skill: &str) -> SkillConfig {
        self.host
            .skill_config
            .get(skill)
            .cloned()
            .unwrap_or_default()
    }

    /// Resolve which LLM backend `skill` should use.
    ///
    /// Fallback chain (first match wins):
    /// 1. `[host.skill_config.{skill}].llm_backend` — per-skill override.
    /// 2. `[host].default_llm_backend` — persona-wide default.
    /// 3. `FALLBACK_LLM_BACKEND` (`"ollama"`) — ultimate fallback so
    ///    airtime keeps working with no config + no API keys.
    pub fn resolve_llm_backend(&self, skill: &str) -> String {
        self.host
            .skill_config
            .get(skill)
            .and_then(|cfg| cfg.llm_backend.clone())
            .or_else(|| self.host.default_llm_backend.clone())
            .unwrap_or_else(|| FALLBACK_LLM_BACKEND.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const DONNA: &str = r#"
[host]
name         = "Donna Wavelength"
callsign     = "KFLT"
era          = "1960s"
genre        = ["jazz", "soul"]
voice_model  = "af_heart"
tone_prompt  = "Warm, slightly sardonic."

[host.skills]
track_intro = true
top_of_hour = true
weather     = true

[host.skill_config.top_of_hour]
max_words    = 80
llm_backend  = "claude"
news_sources = ["https://feeds.npr.org/1001/rss.xml"]

[host.skill_config.weather]
max_words    = 40
llm_backend  = "ollama"

[host.audio]
eq_profile      = "warm_analog"
room_tone       = true
loudness_target = -14

[stream]
mount = "/donna"
format = "flac"
"#;

    #[test]
    fn parses_persona() {
        let persona: Persona = toml::from_str(DONNA).expect("parse");
        assert_eq!(persona.host.name, "Donna Wavelength");
        assert_eq!(persona.host.callsign, "KFLT");
        assert_eq!(persona.host.genre, vec!["jazz", "soul"]);
        assert_eq!(persona.stream.mount, "/donna");
        assert!(persona.stream.bitrate.is_none());
    }

    #[test]
    fn enabled_skills_in_declaration_order() {
        let persona: Persona = toml::from_str(DONNA).expect("parse");
        assert_eq!(
            persona.host.skills.enabled(),
            vec!["track_intro", "top_of_hour", "weather"]
        );
    }

    #[test]
    fn skill_config_overrides() {
        let persona: Persona = toml::from_str(DONNA).expect("parse");
        let cfg = persona.skill_config("top_of_hour");
        assert_eq!(cfg.max_words, 80);
        assert_eq!(cfg.llm_backend.as_deref(), Some("claude"));
        assert_eq!(cfg.news_sources.len(), 1);
    }

    #[test]
    fn skill_config_defaults_when_missing() {
        let persona: Persona = toml::from_str(DONNA).expect("parse");
        let cfg = persona.skill_config("track_intro");
        assert_eq!(cfg.max_words, 40);
        // Per-skill `llm_backend` is None — caller resolves via
        // `resolve_llm_backend` to walk the persona → fallback chain.
        assert!(cfg.llm_backend.is_none());
    }

    /// Tiny persona with no `default_llm_backend` and no per-skill
    /// overrides — verifies the ultimate `"ollama"` fallback fires.
    const BARE: &str = r#"
[host]
name         = "X"
callsign     = "X"
era          = "X"
genre        = []
voice_model  = "X"
tone_prompt  = "X"

[host.skills]

[host.audio]
loudness_target = -14.0

[stream]
mount  = "/x"
format = "flac"
"#;

    #[test]
    fn resolve_llm_backend_per_skill_wins() {
        let persona: Persona = toml::from_str(DONNA).expect("parse");
        // DONNA sets top_of_hour to claude explicitly.
        assert_eq!(persona.resolve_llm_backend("top_of_hour"), "claude");
    }

    #[test]
    fn resolve_llm_backend_falls_through_to_host_default() {
        let persona: Persona = toml::from_str(
            r#"
[host]
name         = "X"
callsign     = "X"
era          = "X"
genre        = []
voice_model  = "X"
tone_prompt  = "X"
default_llm_backend = "openrouter"

[host.skills]

[host.audio]
loudness_target = -14.0

[stream]
mount  = "/x"
format = "flac"
"#,
        )
        .expect("parse");
        assert_eq!(persona.resolve_llm_backend("station_id"), "openrouter");
        assert_eq!(persona.resolve_llm_backend("weather"), "openrouter");
    }

    #[test]
    fn resolve_llm_backend_per_skill_still_overrides_host_default() {
        let persona: Persona = toml::from_str(
            r#"
[host]
name         = "X"
callsign     = "X"
era          = "X"
genre        = []
voice_model  = "X"
tone_prompt  = "X"
default_llm_backend = "openrouter"

[host.skills]

[host.skill_config.weather]
llm_backend = "claude"

[host.audio]
loudness_target = -14.0

[stream]
mount  = "/x"
format = "flac"
"#,
        )
        .expect("parse");
        // weather has its own override:
        assert_eq!(persona.resolve_llm_backend("weather"), "claude");
        // station_id inherits the host default:
        assert_eq!(persona.resolve_llm_backend("station_id"), "openrouter");
    }

    #[test]
    fn resolve_llm_backend_ultimate_fallback_is_ollama() {
        let persona: Persona = toml::from_str(BARE).expect("parse");
        assert_eq!(
            persona.resolve_llm_backend("anything"),
            FALLBACK_LLM_BACKEND
        );
        assert_eq!(persona.resolve_llm_backend("anything"), "ollama");
    }

    const SETTINGS: &str = r#"
[icecast]
host     = "localhost"
port     = 8000
password = "hackme"

[ollama]
base_url = "http://localhost:11434"
model    = "llama3.1:8b"

[claude]
model    = "claude-sonnet-4-20250514"

[kokoro]
model_path  = "./models/kokoro-v1.0.onnx"
voices_path = "./models/voices.bin"

[library]
path    = "/music"
formats = ["flac", "wav", "aiff"]

[feeds]
weather_lat = 47.6062
weather_lon = -122.3321

[feeds.traffic]
provider = "tomtom"

[audio]
temp_dir        = "/tmp/airtime"
loudness_target = -14.0
true_peak       = -1.0
"#;

    #[test]
    fn parses_settings() {
        let s: Settings = toml::from_str(SETTINGS).expect("parse");
        assert_eq!(s.icecast.host, "localhost");
        assert_eq!(s.icecast.user, "source");
        assert_eq!(s.kokoro.sample_rate, 24_000);
        assert_eq!(s.library.formats, vec!["flac", "wav", "aiff"]);
        assert!(s.library.scan_on_start);
        assert_eq!(s.feeds.traffic.as_ref().unwrap().provider, "tomtom");
        // `[openrouter]` is optional — the fixture omits it.
        assert!(s.openrouter.is_none());
    }

    #[test]
    fn parses_settings_with_openrouter_block() {
        let with_or =
            format!("{SETTINGS}\n[openrouter]\nmodel = \"anthropic/claude-3.5-sonnet\"\n");
        let s: Settings = toml::from_str(&with_or).expect("parse");
        let or = s.openrouter.expect("openrouter present");
        assert_eq!(or.model, "anthropic/claude-3.5-sonnet");
        assert!(or.base_url.is_none());
    }
}
