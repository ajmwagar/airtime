//! Self-hosted music library scanner.
//!
//! Builds an in-memory index of every track in the library directory.
//! Lossless formats only (FLAC/WAV/AIFF) — refuses to index lossy files
//! even if they're present in the directory.

pub mod scanner;

pub use scanner::{LibraryError, MusicLibrary, Track};
