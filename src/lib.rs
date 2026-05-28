//! Airtime — personal AI radio station platform.
//!
//! Each station owns an independent tokio task graph: it generates DJ
//! segments via LLM + TTS, sequences them with music from the library,
//! and pushes the result to an Icecast2 mount.

pub mod audio;
pub mod config;
pub mod feeds;
pub mod library;
pub mod llm;
pub mod mixer;
pub mod scheduler;
pub mod skills;
pub mod stream;
pub mod tts;
