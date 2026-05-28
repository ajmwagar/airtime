//! Cached external feeds (news, weather, traffic).
//!
//! Each feed is refreshed on its own schedule and exposes a snapshot the
//! skills can read synchronously.

pub mod news;
pub mod traffic;
pub mod weather;

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Default, Serialize)]
pub struct FeedSnapshot {
    pub news: Vec<news::NewsItem>,
    pub weather: Option<weather::WeatherNow>,
    pub traffic: Option<traffic::TrafficNow>,
    pub fetched_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Default)]
pub struct FeedCache {
    inner: Arc<RwLock<FeedSnapshot>>,
}

impl FeedCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> FeedSnapshot {
        self.inner.read().await.clone()
    }

    pub async fn set_news(&self, items: Vec<news::NewsItem>) {
        let mut guard = self.inner.write().await;
        guard.news = items;
        guard.fetched_at = Some(Utc::now());
    }

    pub async fn set_weather(&self, w: weather::WeatherNow) {
        let mut guard = self.inner.write().await;
        guard.weather = Some(w);
        guard.fetched_at = Some(Utc::now());
    }

    pub async fn set_traffic(&self, t: traffic::TrafficNow) {
        let mut guard = self.inner.write().await;
        guard.traffic = Some(t);
        guard.fetched_at = Some(Utc::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_stores_and_returns_news() {
        let cache = FeedCache::new();
        let item = news::NewsItem {
            title: "Big news".into(),
            summary: "Stuff happened".into(),
            source: "https://example.com/feed".into(),
        };
        cache.set_news(vec![item.clone()]).await;
        let snap = cache.snapshot().await;
        assert_eq!(snap.news.len(), 1);
        assert_eq!(snap.news[0].title, "Big news");
        assert!(snap.fetched_at.is_some());
    }

    #[tokio::test]
    async fn cache_stores_weather_and_traffic() {
        let cache = FeedCache::new();
        cache
            .set_weather(weather::WeatherNow {
                temperature_c: 12.0,
                conditions: "cloudy".into(),
                wind_kph: 8.0,
            })
            .await;
        cache
            .set_traffic(traffic::TrafficNow {
                summary: "I-5 sluggish".into(),
                incidents: 2,
            })
            .await;
        let snap = cache.snapshot().await;
        assert_eq!(snap.weather.as_ref().unwrap().conditions, "cloudy");
        assert_eq!(snap.traffic.as_ref().unwrap().incidents, 2);
    }
}
