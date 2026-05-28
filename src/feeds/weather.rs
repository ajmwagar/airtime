//! Open-Meteo weather fetcher.
//!
//! Open-Meteo is a free, no-key service that returns hourly + current
//! conditions. We only consume the `current_weather` block.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeatherNow {
    pub temperature_c: f64,
    pub conditions: String,
    pub wind_kph: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum WeatherError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("malformed weather response: {0}")]
    Malformed(String),
}

pub struct WeatherFetcher {
    http: reqwest::Client,
    base_url: String,
}

impl WeatherFetcher {
    pub fn new() -> Self {
        Self::with_base_url("https://api.open-meteo.com")
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            base_url: base_url.into(),
        }
    }

    pub async fn fetch(&self, lat: f64, lon: f64) -> Result<WeatherNow, WeatherError> {
        let url = format!(
            "{}/v1/forecast?latitude={lat}&longitude={lon}&current_weather=true",
            self.base_url.trim_end_matches('/')
        );
        let raw: serde_json::Value = self.http.get(&url).send().await?.json().await?;
        let cw = raw
            .get("current_weather")
            .ok_or_else(|| WeatherError::Malformed("no current_weather block".into()))?;
        let temperature_c = cw
            .get("temperature")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| WeatherError::Malformed("no temperature".into()))?;
        let wind_kph = cw.get("windspeed").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let code = cw.get("weathercode").and_then(|v| v.as_i64()).unwrap_or(-1);
        Ok(WeatherNow {
            temperature_c,
            wind_kph,
            conditions: weather_code_to_text(code).into(),
        })
    }
}

impl Default for WeatherFetcher {
    fn default() -> Self {
        Self::new()
    }
}

/// WMO weather codes → human text. Reference:
/// https://open-meteo.com/en/docs (current_weather → weathercode).
pub fn weather_code_to_text(code: i64) -> &'static str {
    match code {
        0 => "clear",
        1 | 2 => "mostly clear",
        3 => "overcast",
        45 | 48 => "foggy",
        51 | 53 | 55 => "drizzle",
        61 | 63 | 65 => "rain",
        71 | 73 | 75 => "snow",
        77 => "snow grains",
        80..=82 => "rain showers",
        85 | 86 => "snow showers",
        95 => "thunderstorms",
        96 | 99 => "thunderstorms with hail",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn fetches_current_weather() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("latitude", "47.6062"))
            .and(query_param("longitude", "-122.3321"))
            .and(query_param("current_weather", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "current_weather": {
                    "temperature": 12.3,
                    "windspeed": 14.5,
                    "weathercode": 3
                }
            })))
            .mount(&server)
            .await;

        let fetcher = WeatherFetcher::with_base_url(server.uri());
        let w = fetcher.fetch(47.6062, -122.3321).await.unwrap();
        assert_eq!(w.temperature_c, 12.3);
        assert_eq!(w.wind_kph, 14.5);
        assert_eq!(w.conditions, "overcast");
    }

    #[test]
    fn weather_codes_map() {
        assert_eq!(weather_code_to_text(0), "clear");
        assert_eq!(weather_code_to_text(63), "rain");
        assert_eq!(weather_code_to_text(9999), "unknown");
    }

    #[tokio::test]
    async fn malformed_response_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "unrelated": true
            })))
            .mount(&server)
            .await;
        let fetcher = WeatherFetcher::with_base_url(server.uri());
        let err = fetcher.fetch(0.0, 0.0).await.unwrap_err();
        assert!(matches!(err, WeatherError::Malformed(_)));
    }
}
