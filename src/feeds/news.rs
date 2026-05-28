//! RSS news fetcher.
//!
//! Phase 1: poll RSS 2.0 feeds, extract `<title>` + `<description>` from
//! each `<item>`, cap to ~5 latest stories per feed. Multi-feed support
//! so a persona can pull from several sources.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsItem {
    pub title: String,
    pub summary: String,
    pub source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum NewsError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("malformed RSS from {feed_url}: {detail}")]
    Malformed { feed_url: String, detail: String },
}

pub struct NewsFetcher {
    http: reqwest::Client,
    max_items: usize,
}

impl NewsFetcher {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            max_items: 5,
        }
    }

    pub fn with_max_items(mut self, n: usize) -> Self {
        self.max_items = n;
        self
    }

    pub async fn fetch(&self, source: &str) -> Result<Vec<NewsItem>, NewsError> {
        let body = self.http.get(source).send().await?.text().await?;
        parse_rss(&body, source, self.max_items)
    }

    pub async fn fetch_all(&self, sources: &[String]) -> Vec<NewsItem> {
        let mut out = Vec::new();
        for src in sources {
            match self.fetch(src).await {
                Ok(mut items) => out.append(&mut items),
                Err(e) => tracing::warn!(source = %src, error = %e, "news fetch failed"),
            }
        }
        out
    }
}

impl Default for NewsFetcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Minimal RSS 2.0 parser — `<channel><item><title>…</title><description>…</description></item>`.
///
/// We use `quick-xml`'s event-based reader because RSS feeds in the wild
/// have all kinds of weirdness (CDATA, namespaces, missing fields) and a
/// strict deserializer chokes on most of them.
fn parse_rss(body: &str, source: &str, max_items: usize) -> Result<Vec<NewsItem>, NewsError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);

    let mut items = Vec::new();
    let mut in_item = false;
    let mut current_tag: Option<String> = None;
    let mut title = String::new();
    let mut summary = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" {
                    in_item = true;
                    title.clear();
                    summary.clear();
                } else if in_item {
                    current_tag = Some(name);
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" {
                    in_item = false;
                    if !title.is_empty() {
                        items.push(NewsItem {
                            title: title.trim().to_string(),
                            summary: summary.trim().to_string(),
                            source: source.to_string(),
                        });
                        if items.len() >= max_items {
                            return Ok(items);
                        }
                    }
                }
                current_tag = None;
            }
            Ok(Event::Text(t)) => {
                if in_item {
                    let text = String::from_utf8_lossy(t.as_ref()).into_owned();
                    match current_tag.as_deref() {
                        Some("title") => title.push_str(&text),
                        Some("description") => summary.push_str(&text),
                        _ => {}
                    }
                }
            }
            Ok(Event::CData(t)) => {
                if in_item {
                    let text = String::from_utf8_lossy(t.as_ref()).into_owned();
                    match current_tag.as_deref() {
                        Some("title") => title.push_str(&text),
                        Some("description") => summary.push_str(&text),
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(NewsError::Malformed {
                    feed_url: source.into(),
                    detail: e.to_string(),
                })
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FEED: &str = r#"<?xml version="1.0"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <item>
      <title>Story One</title>
      <description>Summary one.</description>
    </item>
    <item>
      <title>Story Two</title>
      <description><![CDATA[Summary <b>two</b>.]]></description>
    </item>
    <item>
      <title>Story Three</title>
      <description>Summary three.</description>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn parses_items() {
        let items = parse_rss(FEED, "https://example.com/feed", 10).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].title, "Story One");
        assert_eq!(items[1].summary, "Summary <b>two</b>.");
        assert_eq!(items[2].source, "https://example.com/feed");
    }

    #[test]
    fn respects_max_items() {
        let items = parse_rss(FEED, "src", 2).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn fetches_from_http() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(FEED)
                    .insert_header("content-type", "application/rss+xml"),
            )
            .mount(&server)
            .await;

        let fetcher = NewsFetcher::new();
        let items = fetcher.fetch(&server.uri()).await.unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].title, "Story One");
    }

    #[tokio::test]
    async fn fetch_all_continues_on_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FEED))
            .mount(&server)
            .await;

        let fetcher = NewsFetcher::new();
        let sources = vec!["http://127.0.0.1:1/nope".to_string(), server.uri()];
        let items = fetcher.fetch_all(&sources).await;
        assert_eq!(items.len(), 3);
    }
}
