//! OpenRouter chat-completions client.
//!
//! OpenRouter is an OpenAI-compatible gateway that proxies to ~hundreds
//! of upstream models (Anthropic, Google, Meta, Mistral, …) under a
//! single API + key. From airtime's perspective it's just a third
//! `LlmBackend` — the wire format is identical to OpenAI Chat
//! Completions, so the request/response shape mirrors the OpenAI SDK.
//!
//! Auth: `OPENROUTER_API_KEY` env var (Bearer token).
//! Optional attribution headers (`HTTP-Referer`, `X-Title`) help us show
//! up in OpenRouter's per-app dashboards.

use super::{LlmBackend, LlmError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

pub struct OpenRouterClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    /// Attribution header values — surfaced to OpenRouter's dashboard.
    referer: String,
    title: String,
}

impl OpenRouterClient {
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
            referer: "https://github.com/ajmwagar/airtime".into(),
            title: "Airtime".into(),
        }
    }

    /// Loads `OPENROUTER_API_KEY` from the environment.
    pub fn from_env(model: impl Into<String>) -> Result<Self, std::env::VarError> {
        let key = std::env::var("OPENROUTER_API_KEY")?;
        Ok(Self::new(model, key))
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

#[async_trait]
impl LlmBackend for OpenRouterClient {
    async fn complete(&self, system: &str, user: &str) -> Result<String, LlmError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                Message {
                    role: "system",
                    content: system,
                },
                Message {
                    role: "user",
                    content: user,
                },
            ],
        };
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", &self.referer)
            .header("X-Title", &self.title)
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
        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Malformed(e.to_string()))?;
        let text = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Malformed("no choices in response".into()))?
            .message
            .content;
        if text.is_empty() {
            return Err(LlmError::Malformed("empty content in choice".into()));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn sends_chat_completions_request_with_bearer_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .and(header(
                "http-referer",
                "https://github.com/ajmwagar/airtime",
            ))
            .and(header("x-title", "Airtime"))
            .and(body_partial_json(serde_json::json!({
                "model": "anthropic/claude-3.5-sonnet"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "gen-1",
                "object": "chat.completion",
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": "good evening Seattle"},
                        "finish_reason": "stop"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let client =
            OpenRouterClient::with_base_url(server.uri(), "anthropic/claude-3.5-sonnet", "sk-test");
        let out = client.complete("be brief", "intro").await.unwrap();
        assert_eq!(out, "good evening Seattle");
    }

    #[tokio::test]
    async fn includes_system_and_user_messages() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({
                "messages": [
                    {"role": "system", "content": "be brief"},
                    {"role": "user", "content": "say hi"}
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": "hi"}}]
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::with_base_url(server.uri(), "x", "k");
        let out = client.complete("be brief", "say hi").await.unwrap();
        assert_eq!(out, "hi");
    }

    #[tokio::test]
    async fn surfaces_http_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
            .mount(&server)
            .await;

        let client = OpenRouterClient::with_base_url(server.uri(), "x", "bad-key");
        let err = client.complete("s", "u").await.unwrap_err();
        match err {
            LlmError::Status { status, body } => {
                assert_eq!(status, 401);
                assert!(body.contains("invalid api key"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_response_with_no_choices() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": []
            })))
            .mount(&server)
            .await;

        let client = OpenRouterClient::with_base_url(server.uri(), "x", "k");
        let err = client.complete("s", "u").await.unwrap_err();
        assert!(matches!(err, LlmError::Malformed(_)));
    }
}
