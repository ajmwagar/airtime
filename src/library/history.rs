//! Recently-played track ring buffer.
//!
//! Keeps the last N track paths so the scheduler can avoid immediate
//! repeats. Capacity is fixed at construction; when full, the oldest
//! entry is dropped on the next push. When `capacity == 0`, history is
//! a no-op (every track is considered fresh).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TrackHistory {
    recent: VecDeque<PathBuf>,
    capacity: usize,
}

impl TrackHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            recent: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn record(&mut self, path: PathBuf) {
        if self.capacity == 0 {
            return;
        }
        if self.recent.len() >= self.capacity {
            self.recent.pop_front();
        }
        self.recent.push_back(path);
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.recent.iter().any(|p| p.as_path() == path)
    }

    pub fn len(&self) -> usize {
        self.recent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.recent.is_empty()
    }
}

impl Default for TrackHistory {
    /// Default history retains 32 entries — about two LP sides of mileage.
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
        h.record(p("/a"));
        h.record(p("/b"));
        assert!(h.contains(&p("/a")));
        assert!(h.contains(&p("/b")));
        assert!(!h.contains(&p("/c")));
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn evicts_oldest_when_full() {
        let mut h = TrackHistory::new(2);
        h.record(p("/a"));
        h.record(p("/b"));
        h.record(p("/c"));
        assert!(!h.contains(&p("/a")));
        assert!(h.contains(&p("/b")));
        assert!(h.contains(&p("/c")));
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn zero_capacity_disables_history() {
        let mut h = TrackHistory::new(0);
        h.record(p("/a"));
        h.record(p("/b"));
        assert!(h.is_empty());
        assert!(!h.contains(&p("/a")));
    }

    #[test]
    fn default_capacity_is_reasonable() {
        let h = TrackHistory::default();
        assert_eq!(h.capacity, 32);
    }
}
