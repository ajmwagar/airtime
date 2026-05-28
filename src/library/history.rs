//! Recently-played track ring buffer.
//!
//! Keeps two parallel rings: track paths and artist names. Path history
//! prevents the same track repeating; the (smaller) artist history spaces
//! out same-artist back-to-back. Both rings drop the oldest entry on
//! overflow. `capacity == 0` on either ring disables that ring.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TrackHistory {
    recent: VecDeque<PathBuf>,
    path_capacity: usize,
    recent_artists: VecDeque<String>,
    artist_capacity: usize,
}

impl TrackHistory {
    /// `path_capacity` sets the track-path ring. Artist ring sizes itself
    /// to a quarter of that (min 3) — wide enough to space out an artist's
    /// hits, narrow enough not to starve a small library.
    pub fn new(path_capacity: usize) -> Self {
        let artist_capacity = (path_capacity / 4).max(3);
        Self::with_capacities(path_capacity, artist_capacity)
    }

    pub fn with_capacities(path_capacity: usize, artist_capacity: usize) -> Self {
        Self {
            recent: VecDeque::with_capacity(path_capacity),
            path_capacity,
            recent_artists: VecDeque::with_capacity(artist_capacity),
            artist_capacity,
        }
    }

    /// Record a played track. Empty / "Unknown Artist" tags don't go into
    /// the artist ring — they'd cluster every untagged track together.
    pub fn record(&mut self, path: PathBuf, artist: &str) {
        if self.path_capacity > 0 {
            if self.recent.len() >= self.path_capacity {
                self.recent.pop_front();
            }
            self.recent.push_back(path);
        }
        if self.artist_capacity > 0 && !artist.is_empty() && artist != "Unknown Artist" {
            // Dedup-on-write: re-playing an artist moves them to the back
            // of the queue rather than stacking duplicates, so the ring
            // tracks the last N *distinct* artists played.
            let needle = artist.to_ascii_lowercase();
            self.recent_artists.retain(|a| a != &needle);
            if self.recent_artists.len() >= self.artist_capacity {
                self.recent_artists.pop_front();
            }
            self.recent_artists.push_back(needle);
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.recent.iter().any(|p| p.as_path() == path)
    }

    pub fn contains_artist(&self, artist: &str) -> bool {
        if artist.is_empty() {
            return false;
        }
        let needle = artist.to_ascii_lowercase();
        self.recent_artists.iter().any(|a| a == &needle)
    }

    pub fn len(&self) -> usize {
        self.recent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.recent.is_empty()
    }
}

impl Default for TrackHistory {
    /// Default history retains 32 path entries and ~8 artists.
    fn default() -> Self {
        Self::new(32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn records_and_recalls() {
        let mut h = TrackHistory::new(4);
        h.record(p("/a"), "alice");
        h.record(p("/b"), "bob");
        assert!(h.contains(&p("/a")));
        assert!(h.contains(&p("/b")));
        assert!(!h.contains(&p("/c")));
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn evicts_oldest_when_full() {
        let mut h = TrackHistory::new(2);
        h.record(p("/a"), "alice");
        h.record(p("/b"), "bob");
        h.record(p("/c"), "carol");
        assert!(!h.contains(&p("/a")));
        assert!(h.contains(&p("/b")));
        assert!(h.contains(&p("/c")));
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn zero_capacity_disables_history() {
        let mut h = TrackHistory::with_capacities(0, 0);
        h.record(p("/a"), "alice");
        h.record(p("/b"), "bob");
        assert!(h.is_empty());
        assert!(!h.contains(&p("/a")));
        assert!(!h.contains_artist("alice"));
    }

    #[test]
    fn default_capacity_is_reasonable() {
        let h = TrackHistory::default();
        assert_eq!(h.path_capacity, 32);
        assert_eq!(h.artist_capacity, 8);
    }

    /// Artist ring spaces out same-artist plays. Three-artist library:
    /// after recording all three, the oldest artist should evict first.
    #[test]
    fn artist_ring_evicts_in_order() {
        let mut h = TrackHistory::with_capacities(32, 2);
        h.record(p("/a"), "Miles Davis");
        h.record(p("/b"), "John Coltrane");
        assert!(h.contains_artist("miles davis"));
        assert!(h.contains_artist("John Coltrane"));
        h.record(p("/c"), "Bill Evans");
        assert!(!h.contains_artist("Miles Davis"));
        assert!(h.contains_artist("John Coltrane"));
        assert!(h.contains_artist("Bill Evans"));
    }

    /// Unknown / empty artists do not pollute the artist ring — otherwise
    /// every untagged track would block every other untagged track.
    #[test]
    fn unknown_artist_not_tracked() {
        let mut h = TrackHistory::new(8);
        h.record(p("/a"), "Unknown Artist");
        h.record(p("/b"), "");
        assert!(!h.contains_artist("Unknown Artist"));
        assert!(!h.contains_artist(""));
    }

    /// Case-insensitive: "Miles Davis" and "miles davis" are the same artist.
    #[test]
    fn artist_match_is_case_insensitive() {
        let mut h = TrackHistory::new(8);
        h.record(p("/a"), "Miles Davis");
        assert!(h.contains_artist("MILES DAVIS"));
        assert!(h.contains_artist("miles davis"));
    }

    /// Repeat plays of the same artist don't waste ring slots — the ring
    /// stays an N-distinct list, with the newest play moved to the back.
    /// Without this, an artist-heavy run would push the other artist out
    /// prematurely.
    #[test]
    fn repeated_artist_deduplicates_in_ring() {
        let mut h = TrackHistory::with_capacities(32, 2);
        h.record(p("/a"), "Miles Davis");
        h.record(p("/b"), "John Coltrane");
        h.record(p("/c"), "Miles Davis"); // re-played
                                          // Coltrane must NOT have been evicted by the Miles repeat.
        assert!(h.contains_artist("John Coltrane"));
        assert!(h.contains_artist("Miles Davis"));
        // Adding a third *distinct* artist evicts the now-oldest (Coltrane).
        h.record(p("/d"), "Bill Evans");
        assert!(!h.contains_artist("John Coltrane"));
        assert!(h.contains_artist("Miles Davis"));
        assert!(h.contains_artist("Bill Evans"));
    }
}
