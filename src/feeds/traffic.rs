//! TomTom traffic incidents fetcher.
//!
//! Reads the `incidentDetails` endpoint and reduces it to a one-line
//! summary the DJ can mention. We don't try to render every incident —
//! skills can ask for the count and pick representative items.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficNow {
    pub summary: String,
    pub incidents: usize,
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
            "{}/traffic/services/5/incidentDetails?bbox={min_lon},{min_lat},{max_lon},{max_lat}&fields=%7Bincidents%7Bproperties%7BiconCategory%7D%7D%7D&key={}",
            self.base_url.trim_end_matches('/'),
            self.api_key,
        );
        let raw: serde_json::Value = self.http.get(&url).send().await?.json().await?;
        let count = incident_count(&raw)?;
        let summary = if count == 0 {
            "Roads are clear.".into()
        } else if count == 1 {
            "One incident reported.".into()
        } else {
            format!("{count} incidents reported.")
        };
        Ok(TrafficNow {
            summary,
            incidents: count,
        })
    }
}

/// Counts incidents in a TomTom v5 `incidentDetails` response.
///
/// TomTom returns one of three real shapes depending on the request and
/// the account's plan; we tolerate all of them.
///
/// 1. **Keyword-filtered** (when `fields={incidents{…}}` is passed —
///    which is what airtime sends):  `{ "incidents": [...] }`
/// 2. **Default GeoJSON**: `{ "type": "FeatureCollection", "features": [...] }`
/// 3. **Error envelope**: `{ "detailedError": { "message": "..." } }`
///    (or `{"error": {"description": "..."}}` on older edges).
fn incident_count(raw: &serde_json::Value) -> Result<usize, TrafficError> {
    if let Some(arr) = raw.get("incidents").and_then(|v| v.as_array()) {
        return Ok(arr.len());
    }
    if let Some(arr) = raw.get("features").and_then(|v| v.as_array()) {
        return Ok(arr.len());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn fetches_incident_count() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/traffic/services/5/incidentDetails$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "incidents": [
                    {"properties": {"iconCategory": 6}},
                    {"properties": {"iconCategory": 8}}
                ]
            })))
            .mount(&server)
            .await;

        let fetcher = TrafficFetcher::with_base_url(server.uri(), "test-key");
        let out = fetcher.fetch((-122.5, 47.4, -122.0, 47.8)).await.unwrap();
        assert_eq!(out.incidents, 2);
        assert_eq!(out.summary, "2 incidents reported.");
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
        assert_eq!(out.summary, "Roads are clear.");
    }

    /// TomTom's default response (no `fields` keyword filter) is a
    /// GeoJSON FeatureCollection rather than the `{ incidents: [...] }`
    /// shape. The parser accepts both.
    #[tokio::test]
    async fn accepts_geojson_feature_collection_shape() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "type": "FeatureCollection",
                "features": [
                    {"type": "Feature", "properties": {"iconCategory": 6}, "geometry": {}},
                    {"type": "Feature", "properties": {"iconCategory": 8}, "geometry": {}},
                    {"type": "Feature", "properties": {"iconCategory": 1}, "geometry": {}}
                ]
            })))
            .mount(&server)
            .await;
        let fetcher = TrafficFetcher::with_base_url(server.uri(), "k");
        let out = fetcher.fetch((0.0, 0.0, 1.0, 1.0)).await.unwrap();
        assert_eq!(out.incidents, 3);
        assert_eq!(out.summary, "3 incidents reported.");
    }

    /// TomTom returns a `detailedError` envelope on bad params (e.g.
    /// invalid bbox, expired key). Surface the message instead of the
    /// generic "no incidents array" so operators can act on it.
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

    /// Legacy edge-API error shape: `{"error": {"description": "..."}}`.
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
}
