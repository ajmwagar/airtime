//! Self-hosted music library scanner.
//!
//! Builds an in-memory index of every track in the library directory.
//! Lossless formats only (FLAC/WAV/AIFF) — refuses to index lossy files
//! even if they're present in the directory.

pub mod history;
pub mod scanner;

pub use history::TrackHistory;
pub use scanner::{LibraryError, MusicLibrary, Track};
