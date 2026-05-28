//! Anthropic Claude `/v1/messages` client.
//!
//! Uses the public Messages API directly via reqwest rather than the
//! official SDK, so we keep the dependency footprint small. Auth via
//! `ANTHROPIC_API_KEY` env var by default.

use super::{LlmBackend, LlmError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

pub struct ClaudeClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
}

impl ClaudeClient {
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_base_url(DEFAULT_BASE_URL, model, api_key)
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url: base_url.into(),
            model: model.into(),
            api_key: api_key.into(),
        }
    }

    /// Loads `ANTHROPIC_API_KEY` from the environment.
    pub fn from_env(model: impl Into<String>) -> Result<Self, std::env::VarError> {
        let key = std::env::var("ANTHROPIC_API_KEY")?;
        Ok(Self::new(model, key))
    }
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    system: &'a str,
    messages: Vec<UserMessage<'a>>,
    max_tokens: u32,
}

#[derive(Serialize)]
struct UserMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    text: String,
}

#[async_trait]
impl LlmBackend for ClaudeClient {
    async fn complete(&self, system: &str, user: &str) -> Result<String, LlmError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let body = MessagesRequest {
            model: &self.model,
            system,
            messages: vec![UserMessage {
                role: "user",
                content: user,
            }],
            max_tokens: 1024,
        };
        let resp = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: MessagesResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Malformed(e.to_string()))?;
        let text = parsed
            .content
            .into_iter()
            .filter(|b| b.ty == "text")
            .map(|b| b.text)
            .collect::<Vec<_>>()
            .join("");
        if text.is_empty() {
            return Err(LlmError::Malformed("no text blocks in response".into()));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn sends_messages_request_with_auth_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-test"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [
                    {"type": "text", "text": "good evening, "},
                    {"type": "text", "text": "Seattle"}
                ]
            })))
            .mount(&server)
            .await;

        let client =
            ClaudeClient::with_base_url(server.uri(), "claude-sonnet-4-20250514", "sk-test");
        let out = client.complete("be brief", "intro me").await.unwrap();
        assert_eq!(out, "good evening, Seattle");
    }

    #[tokio::test]
    async fn rejects_response_with_no_text_blocks() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": []
            })))
            .mount(&server)
            .await;

        let client =
            ClaudeClient::with_base_url(server.uri(), "claude-sonnet-4-20250514", "sk-test");
        let err = client.complete("s", "u").await.unwrap_err();
        assert!(matches!(err, LlmError::Malformed(_)));
    }
}
