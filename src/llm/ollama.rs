//! Ollama `/api/chat` client.

use super::{LlmBackend, LlmError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub struct OllamaClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url: base_url.into(),
            model: model.into(),
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    stream: bool,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

#[async_trait]
impl LlmBackend for OllamaClient {
    async fn complete(&self, system: &str, user: &str) -> Result<String, LlmError> {
        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
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
            stream: false,
        };
        let resp = self.http.post(&url).json(&body).send().await?;
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
        Ok(parsed.message.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn sends_chat_request_and_returns_content() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_partial_json(serde_json::json!({
                "model": "llama3.1:8b",
                "stream": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "llama3.1:8b",
                "message": {"role": "assistant", "content": "hello listeners"}
            })))
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri(), "llama3.1:8b");
        let out = client.complete("be brief", "say hi").await.unwrap();
        assert_eq!(out, "hello listeners");
    }

    #[tokio::test]
    async fn surfaces_http_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("model unloaded"))
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri(), "llama3.1:8b");
        let err = client.complete("s", "u").await.unwrap_err();
        match err {
            LlmError::Status { status, body } => {
                assert_eq!(status, 500);
                assert!(body.contains("model unloaded"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
