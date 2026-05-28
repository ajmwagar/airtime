//! LLM router and backend traits.
//!
//! Each skill picks its backend (`"ollama"` or `"claude"`) via persona
//! config. The router dispatches accordingly. Backends are trait objects
//! so tests can swap in deterministic stubs.

pub mod claude;
pub mod ollama;
pub mod openrouter;

use async_trait::async_trait;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("backend `{0}` not configured")]
    UnknownBackend(String),
    #[error("backend returned malformed response: {0}")]
    Malformed(String),
    #[error("backend returned http {status}: {body}")]
    Status { status: u16, body: String },
}

#[async_trait]
pub trait LlmBackend: Send + Sync {
    /// Single-turn chat completion. Returns the assistant text.
    async fn complete(&self, system: &str, user: &str) -> Result<String, LlmError>;
}

/// Dispatches to the named backend.
pub struct LlmRouter {
    ollama: Arc<dyn LlmBackend>,
    claude: Arc<dyn LlmBackend>,
    openrouter: Arc<dyn LlmBackend>,
}

impl LlmRouter {
    pub fn new(
        ollama: Arc<dyn LlmBackend>,
        claude: Arc<dyn LlmBackend>,
        openrouter: Arc<dyn LlmBackend>,
    ) -> Self {
        Self {
            ollama,
            claude,
            openrouter,
        }
    }

    pub async fn complete(
        &self,
        system: &str,
        user: &str,
        backend: &str,
    ) -> Result<String, LlmError> {
        match backend {
            "claude" => self.claude.complete(system, user).await,
            "openrouter" => self.openrouter.complete(system, user).await,
            "ollama" | "" => self.ollama.complete(system, user).await,
            other => Err(LlmError::UnknownBackend(other.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Records which backend was hit.
    struct TaggedBackend {
        tag: &'static str,
        hits: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LlmBackend for TaggedBackend {
        async fn complete(&self, _system: &str, _user: &str) -> Result<String, LlmError> {
            self.hits.fetch_add(1, Ordering::SeqCst);
            Ok(self.tag.into())
        }
    }

    /// One counter per backend so we can assert routing went where we expected.
    struct Counters {
        ollama: Arc<AtomicUsize>,
        claude: Arc<AtomicUsize>,
        openrouter: Arc<AtomicUsize>,
    }

    fn build_router() -> (LlmRouter, Counters) {
        let ollama = Arc::new(AtomicUsize::new(0));
        let claude = Arc::new(AtomicUsize::new(0));
        let openrouter = Arc::new(AtomicUsize::new(0));
        let router = LlmRouter::new(
            Arc::new(TaggedBackend {
                tag: "ollama",
                hits: ollama.clone(),
            }),
            Arc::new(TaggedBackend {
                tag: "claude",
                hits: claude.clone(),
            }),
            Arc::new(TaggedBackend {
                tag: "openrouter",
                hits: openrouter.clone(),
            }),
        );
        (
            router,
            Counters {
                ollama,
                claude,
                openrouter,
            },
        )
    }

    #[tokio::test]
    async fn routes_to_ollama_by_default() {
        let (router, c) = build_router();
        let out = router.complete("s", "u", "ollama").await.unwrap();
        assert_eq!(out, "ollama");
        assert_eq!(c.ollama.load(Ordering::SeqCst), 1);
        assert_eq!(c.claude.load(Ordering::SeqCst), 0);
        assert_eq!(c.openrouter.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn routes_to_claude_when_named() {
        let (router, c) = build_router();
        let out = router.complete("s", "u", "claude").await.unwrap();
        assert_eq!(out, "claude");
        assert_eq!(c.claude.load(Ordering::SeqCst), 1);
        assert_eq!(c.ollama.load(Ordering::SeqCst), 0);
        assert_eq!(c.openrouter.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn routes_to_openrouter_when_named() {
        let (router, c) = build_router();
        let out = router.complete("s", "u", "openrouter").await.unwrap();
        assert_eq!(out, "openrouter");
        assert_eq!(c.openrouter.load(Ordering::SeqCst), 1);
        assert_eq!(c.ollama.load(Ordering::SeqCst), 0);
        assert_eq!(c.claude.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn empty_backend_falls_back_to_ollama() {
        let (router, c) = build_router();
        assert_eq!(router.complete("s", "u", "").await.unwrap(), "ollama");
        assert_eq!(c.ollama.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_backend_errors() {
        let (router, _) = build_router();
        let err = router.complete("s", "u", "gpt").await.unwrap_err();
        assert!(matches!(err, LlmError::UnknownBackend(_)));
    }
}
