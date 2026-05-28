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
        let incidents = raw
            .get("incidents")
            .and_then(|v| v.as_array())
            .ok_or_else(|| TrafficError::Malformed("no incidents array".into()))?;
        let count = incidents.len();
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
        let out = fetcher
            .fetch((-122.5, 47.4, -122.0, 47.8))
            .await
            .unwrap();
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
}
