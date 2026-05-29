//! TomTom traffic incidents fetcher.
//!
//! Reads the `incidentDetails` endpoint and reduces it to a structured
//! summary the DJ can mention by road name. We request enough per-incident
//! detail (magnitudeOfDelay, delay seconds, from/to road labels, events)
//! to let the skill voice specific slowdowns rather than just parroting
//! the raw incident count — a metro-sized bbox can return hundreds of
//! markers, most of which aren't driver-relevant.

use serde::{Deserialize, Serialize};

/// What gets handed to the traffic skill. Keeps `summary` as a short
/// status line for backwards compatibility while exposing structured
/// `worst` incidents the prompt builds against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficNow {
    /// One-line status: "Roads are clear.", "3 active slowdowns ...",
    /// etc. The skill can echo it as-is or build something richer
    /// from the structured fields below.
    pub summary: String,
    /// Total incident count returned by TomTom — includes minor
    /// markers (roadworks, broken-down vehicles, ferry stops). In a
    /// metro bbox this can run into the hundreds, which is why we
    /// also expose the filtered `significant` count.
    pub incidents: usize,
    /// Incidents with `magnitudeOfDelay >= Moderate (2)`. This is the
    /// number drivers actually care about — the rest is noise.
    pub significant: usize,
    /// Total delay across significant incidents, in seconds. The
    /// skill divides by 60 for "about N minutes lost" copy.
    pub total_delay_seconds: u64,
    /// Top 3 significant incidents by delay, descending. The skill
    /// can mention one or two by road name without reciting the list.
    pub worst: Vec<TrafficIncident>,
}

/// One concrete slowdown — road label, optional direction, delay,
/// and any event descriptions ("Accident", "Roadworks", etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficIncident {
    /// Best available road label: the first `roadNumbers` entry, or
    /// the `from` text if road numbers aren't reported, or "an
    /// unnamed road" as a final fallback.
    pub road: String,
    /// Free-form direction hint ("near Mercer", "between X and Y"),
    /// derived from `from`/`to`. Empty when neither is present.
    pub direction: Option<String>,
    pub delay_seconds: u64,
    /// 0 = Unknown, 1 = Minor, 2 = Moderate, 3 = Major, 4 = Undefined.
    pub magnitude: u8,
    /// Event descriptions, e.g. ["Accident"], ["Roadworks", "Lane closure"].
    pub events: Vec<String>,
}

impl TrafficIncident {
    /// One-line prompt-friendly description used by the skill to give
    /// the LLM specific slowdowns to reference. Format:
    /// `"{road}{direction}: {minutes} min, {events}"` with components
    /// elided when missing.
    pub fn one_line(&self) -> String {
        let mut out = self.road.clone();
        if let Some(d) = &self.direction {
            if !d.is_empty() {
                out.push(' ');
                out.push_str(d);
            }
        }
        let minutes = self.delay_seconds / 60;
        if minutes > 0 {
            out.push_str(&format!(": {minutes} min"));
        }
        if !self.events.is_empty() {
            out.push_str(&format!(", {}", self.events.join("/")));
        }
        out
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TrafficError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("malformed traffic response: {0}")]
    Malformed(String),
    #[error("missing API key (set TOMTOM_API_KEY)")]
    MissingKey,
}

pub struct TrafficFetcher {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

/// Fields keyword filter passed to TomTom's incidentDetails endpoint.
/// Asks for the per-incident properties the skill builds prompts
/// against. Plain text — reqwest URL-encodes it for us.
const FIELDS_FILTER: &str = "{incidents{properties{iconCategory,magnitudeOfDelay,roadNumbers,delay,events{description},from,to}}}";

impl TrafficFetcher {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url("https://api.tomtom.com", api_key)
    }

    pub fn with_base_url(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            base_url: base_url.into(),
            api_key: api_key.into(),
        }
    }

    pub fn from_env() -> Result<Self, TrafficError> {
        let key = std::env::var("TOMTOM_API_KEY").map_err(|_| TrafficError::MissingKey)?;
        Ok(Self::new(key))
    }

    /// Fetch incidents inside the given bounding box (minLon,minLat,maxLon,maxLat).
    pub async fn fetch(&self, bbox: (f64, f64, f64, f64)) -> Result<TrafficNow, TrafficError> {
        let (min_lon, min_lat, max_lon, max_lat) = bbox;
        let url = format!(
            "{}/traffic/services/5/incidentDetails",
            self.base_url.trim_end_matches('/'),
        );
        let raw: serde_json::Value = self
            .http
            .get(&url)
            .query(&[
                ("bbox", format!("{min_lon},{min_lat},{max_lon},{max_lat}")),
                ("fields", FIELDS_FILTER.to_string()),
                ("key", self.api_key.clone()),
            ])
            .send()
            .await?
            .json()
            .await?;
        let items = extract_items(&raw)?;
        Ok(build_summary(items))
    }
}

/// Raw incidents extracted from either the `incidents: [...]` shape
/// (keyword-filtered) or the GeoJSON `features: [...]` shape (default).
fn extract_items(raw: &serde_json::Value) -> Result<Vec<TrafficIncident>, TrafficError> {
    let array_ref = raw
        .get("incidents")
        .and_then(|v| v.as_array())
        .or_else(|| raw.get("features").and_then(|v| v.as_array()));
    if let Some(arr) = array_ref {
        return Ok(arr.iter().map(parse_incident).collect());
    }
    // Error envelopes — distinguish from a structurally-bad response so
    // operators see the real reason (bad key, rate limit, etc.).
    if let Some(msg) = raw
        .get("detailedError")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .or_else(|| {
            raw.get("error")
                .and_then(|e| e.get("description").or_else(|| e.get("message")))
                .and_then(|m| m.as_str())
        })
    {
        return Err(TrafficError::Malformed(format!("tomtom error: {msg}")));
    }
    Err(TrafficError::Malformed(format!(
        "no `incidents` or `features` array in response: {raw}"
    )))
}

fn parse_incident(item: &serde_json::Value) -> TrafficIncident {
    let props = item.get("properties").unwrap_or(item);
    let magnitude = props
        .get("magnitudeOfDelay")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(255) as u8)
        .unwrap_or(0);
    let delay_seconds = props.get("delay").and_then(|v| v.as_u64()).unwrap_or(0);
    let road_numbers: Vec<String> = props
        .get("roadNumbers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let from = props
        .get("from")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let to = props.get("to").and_then(|v| v.as_str()).map(str::to_string);
    let events: Vec<String> = props
        .get("events")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("description").and_then(|d| d.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let road = road_numbers
        .first()
        .cloned()
        .or_else(|| from.clone())
        .unwrap_or_else(|| "an unnamed road".into());
    // Compose a brief direction hint from from/to when both are present;
    // otherwise fall back to "near {from}" so the LLM still has a hook.
    let direction = match (from.as_deref(), to.as_deref()) {
        (Some(f), Some(t)) if f != t => Some(format!("between {f} and {t}")),
        (Some(f), _) if road_numbers.first().map(String::as_str) != Some(f) => {
            Some(format!("near {f}"))
        }
        _ => None,
    };

    TrafficIncident {
        road,
        direction,
        delay_seconds,
        magnitude,
        events,
    }
}

/// Roll up a list of parsed incidents into the `TrafficNow` the skill
/// reads. `summary` is short prose; the structured fields back the
/// rich prompt branch.
fn build_summary(items: Vec<TrafficIncident>) -> TrafficNow {
    let incidents = items.len();
    let significant_items: Vec<&TrafficIncident> =
        items.iter().filter(|i| i.magnitude >= 2).collect();
    let significant = significant_items.len();
    let total_delay_seconds: u64 = significant_items.iter().map(|i| i.delay_seconds).sum();

    let mut worst: Vec<TrafficIncident> = significant_items.into_iter().cloned().collect();
    worst.sort_by_key(|i| std::cmp::Reverse(i.delay_seconds));
    worst.truncate(3);

    let summary = if incidents == 0 {
        "Roads are clear.".into()
    } else if significant == 0 {
        "Roads are mostly clear — only minor incidents reported.".into()
    } else {
        let minutes = total_delay_seconds / 60;
        if significant == 1 {
            format!("One active slowdown, about {minutes} minutes of delay.")
        } else {
            format!("{significant} active slowdowns, about {minutes} minutes of cumulative delay across the metro.")
        }
    };

    TrafficNow {
        summary,
        incidents,
        significant,
        total_delay_seconds,
        worst,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper for building a fake incident JSON value in the wire shape.
    fn incident(
        road: &str,
        magnitude: u64,
        delay: u64,
        from: &str,
        to: &str,
        events: &[&str],
    ) -> serde_json::Value {
        serde_json::json!({
            "properties": {
                "iconCategory": 7,
                "magnitudeOfDelay": magnitude,
                "roadNumbers": [road],
                "delay": delay,
                "from": from,
                "to": to,
                "events": events.iter().map(|d| serde_json::json!({"description": d})).collect::<Vec<_>>(),
            }
        })
    }

    #[tokio::test]
    async fn fetches_and_summarises_significant_incidents() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/traffic/services/5/incidentDetails$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "incidents": [
                    incident("I-5", 3, 540, "Mercer St", "Ship Canal Br", &["Accident"]),
                    incident("SR-520", 2, 360, "Montlake", "Foster Island", &["Roadworks"]),
                    // Minor incident — should NOT count toward significant.
                    incident("Local", 1, 60, "Side St", "Other St", &["Broken-down vehicle"]),
                ]
            })))
            .mount(&server)
            .await;

        let fetcher = TrafficFetcher::with_base_url(server.uri(), "test-key");
        let out = fetcher.fetch((-122.5, 47.4, -122.0, 47.8)).await.unwrap();
        assert_eq!(out.incidents, 3);
        assert_eq!(out.significant, 2);
        assert_eq!(out.total_delay_seconds, 900);
        assert_eq!(out.worst.len(), 2);
        assert_eq!(out.worst[0].road, "I-5", "worst should sort by delay desc");
        assert!(
            out.summary.contains("2 active slowdowns"),
            "got: {}",
            out.summary
        );
        assert!(out.summary.contains("15 minutes"), "got: {}", out.summary);
    }

    /// Many-minor scenario — the metro-sized bbox returning 200 markers
    /// of magnitude 0/1 must NOT trigger the "618 incidents out there"
    /// failure mode. The summary should explicitly say "mostly clear".
    #[tokio::test]
    async fn many_minor_incidents_summarises_as_mostly_clear() {
        let server = MockServer::start().await;
        let mut items = Vec::new();
        for _ in 0..200 {
            items.push(incident("Local", 1, 30, "Some St", "Other St", &[]));
        }
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "incidents": items
            })))
            .mount(&server)
            .await;
        let fetcher = TrafficFetcher::with_base_url(server.uri(), "k");
        let out = fetcher.fetch((0.0, 0.0, 1.0, 1.0)).await.unwrap();
        assert_eq!(out.incidents, 200);
        assert_eq!(out.significant, 0);
        assert_eq!(out.worst.len(), 0);
        assert!(out.summary.contains("mostly clear"), "got: {}", out.summary);
    }

    #[tokio::test]
    async fn zero_incidents_returns_clear() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "incidents": []
            })))
            .mount(&server)
            .await;
        let fetcher = TrafficFetcher::with_base_url(server.uri(), "k");
        let out = fetcher.fetch((0.0, 0.0, 1.0, 1.0)).await.unwrap();
        assert_eq!(out.incidents, 0);
        assert_eq!(out.significant, 0);
        assert_eq!(out.worst.len(), 0);
        assert_eq!(out.summary, "Roads are clear.");
    }

    /// GeoJSON shape (no `fields` filter) — parser must still extract
    /// the same structured fields from `features[].properties`.
    #[tokio::test]
    async fn accepts_geojson_feature_collection_shape() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "type": "FeatureCollection",
                "features": [
                    {"type": "Feature", "geometry": {}, "properties": {
                        "iconCategory": 7,
                        "magnitudeOfDelay": 3,
                        "roadNumbers": ["I-405"],
                        "delay": 720,
                        "from": "Bellevue",
                        "to": "Renton",
                        "events": [{"description": "Accident"}]
                    }}
                ]
            })))
            .mount(&server)
            .await;
        let fetcher = TrafficFetcher::with_base_url(server.uri(), "k");
        let out = fetcher.fetch((0.0, 0.0, 1.0, 1.0)).await.unwrap();
        assert_eq!(out.incidents, 1);
        assert_eq!(out.significant, 1);
        assert_eq!(out.worst[0].road, "I-405");
        assert_eq!(out.worst[0].delay_seconds, 720);
    }

    /// TomTom returns a `detailedError` envelope on bad params (e.g.
    /// invalid bbox, expired key). Surface the message.
    #[tokio::test]
    async fn surfaces_tomtom_detailed_error_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "detailedError": {
                    "code": "InvalidApiKey",
                    "message": "API key disabled"
                }
            })))
            .mount(&server)
            .await;
        let fetcher = TrafficFetcher::with_base_url(server.uri(), "k");
        let err = fetcher.fetch((0.0, 0.0, 1.0, 1.0)).await.unwrap_err();
        match err {
            TrafficError::Malformed(msg) => {
                assert!(msg.contains("tomtom error"), "got: {msg}");
                assert!(msg.contains("API key disabled"), "got: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn surfaces_legacy_error_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": {"description": "Rate limit exceeded"}
            })))
            .mount(&server)
            .await;
        let fetcher = TrafficFetcher::with_base_url(server.uri(), "k");
        let err = fetcher.fetch((0.0, 0.0, 1.0, 1.0)).await.unwrap_err();
        assert!(matches!(err, TrafficError::Malformed(msg) if msg.contains("Rate limit exceeded")));
    }

    /// `TrafficIncident::one_line` formats road + direction + minutes +
    /// events. Used directly in the skill prompt — locking the shape so
    /// a refactor can't silently drop the event description.
    #[test]
    fn incident_one_line_includes_minutes_and_event() {
        let inc = TrafficIncident {
            road: "I-5".into(),
            direction: Some("near Mercer".into()),
            delay_seconds: 540,
            magnitude: 3,
            events: vec!["Accident".into()],
        };
        let line = inc.one_line();
        assert!(line.contains("I-5"), "{line}");
        assert!(line.contains("near Mercer"), "{line}");
        assert!(line.contains("9 min"), "{line}");
        assert!(line.contains("Accident"), "{line}");
    }
}
