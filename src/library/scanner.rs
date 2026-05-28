//! Recursive music library scanner.
//!
//! Reads ID3/Vorbis/RIFF tags via `lofty`, builds an in-memory index of
//! all tracks. The scanner only admits **lossless** formats — Phase 1 is
//! lossless end-to-end, so MP3/AAC files in the library are skipped with
//! a warning.

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::Accessor;
use rand::seq::SliceRandom;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use walkdir::WalkDir;

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("library root not found: {0}")]
    NotFound(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<u32>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub duration_ms: u64,
    pub format: String,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
}

/// Lossless formats we admit.
const LOSSLESS_EXTS: &[&str] = &["flac", "wav", "wave", "aif", "aiff"];

fn is_lossless(ext: &str) -> bool {
    LOSSLESS_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

#[derive(Clone, Default)]
pub struct MusicLibrary {
    inner: Arc<RwLock<Vec<Track>>>,
}

impl MusicLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn scan(&self, root: impl AsRef<Path>) -> Result<usize, LibraryError> {
        let root = root.as_ref();
        if !root.exists() {
            return Err(LibraryError::NotFound(root.into()));
        }
        let root = root.to_path_buf();
        // Filesystem walk is blocking — run on a blocking pool.
        let tracks = tokio::task::spawn_blocking(move || walk(&root))
            .await
            .expect("scanner join");
        let count = tracks.len();
        *self.inner.write().await = tracks;
        Ok(count)
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    pub async fn pick_random(&self) -> Option<Track> {
        let guard = self.inner.read().await;
        guard.choose(&mut rand::thread_rng()).cloned()
    }

    /// Random pick that avoids anything in `history`. Cascading
    /// preference: tracks whose path AND artist are both fresh win;
    /// then tracks merely fresh on path; finally any track. The cascade
    /// keeps small libraries playable while still spacing out repeats
    /// when there's room to be picky.
    pub async fn pick_random_avoiding(
        &self,
        history: &super::history::TrackHistory,
    ) -> Option<Track> {
        let guard = self.inner.read().await;
        if guard.is_empty() {
            return None;
        }
        let path_fresh: Vec<&Track> = guard
            .iter()
            .filter(|t| !history.contains(&t.path))
            .collect();
        let fully_fresh: Vec<&Track> = path_fresh
            .iter()
            .copied()
            .filter(|t| !history.contains_artist(&t.artist))
            .collect();
        let pool = if !fully_fresh.is_empty() {
            fully_fresh
        } else if !path_fresh.is_empty() {
            path_fresh
        } else {
            guard.iter().collect::<Vec<_>>()
        };
        pool.choose(&mut rand::thread_rng()).map(|t| (*t).clone())
    }

    pub async fn all(&self) -> Vec<Track> {
        self.inner.read().await.clone()
    }

    /// Genre-aware pick. Honours `mode`:
    ///   - `"strict"`  → only tracks tagged with one of `genres`;
    ///   - `"blended"` → ~80 % genre-matched, ~20 % wildcards;
    ///   - `"free"`    → ignore `genres`, just avoid the history.
    ///
    /// Falls back gracefully when the strict pool would be empty
    /// (no tagged tracks, or every match is in the history): degrades
    /// to a wildcard pick instead of refusing to return anything.
    pub async fn pick(
        &self,
        history: &super::history::TrackHistory,
        genres: &[String],
        mode: &str,
    ) -> Option<Track> {
        use rand::Rng;

        let guard = self.inner.read().await;
        if guard.is_empty() {
            return None;
        }

        let fresh: Vec<&Track> = guard
            .iter()
            .filter(|t| !history.contains(&t.path))
            .collect();
        let wildcard_pool: Vec<&Track> = if fresh.is_empty() {
            guard.iter().collect()
        } else {
            fresh
        };

        let want_filter = matches!(mode, "strict" | "blended") && !genres.is_empty();
        if !want_filter {
            return wildcard_pool
                .choose(&mut rand::thread_rng())
                .map(|t| (*t).clone());
        }

        // 20 % of the time in blended mode, ignore the filter so the
        // station stays surprising.
        if mode == "blended" && rand::thread_rng().gen_bool(0.20) {
            return wildcard_pool
                .choose(&mut rand::thread_rng())
                .map(|t| (*t).clone());
        }

        let matched: Vec<&Track> = wildcard_pool
            .iter()
            .copied()
            .filter(|t| track_matches_genres(t, genres))
            .collect();

        // Fall through if nothing matches. Better to play something off-genre
        // than to play silence.
        if matched.is_empty() {
            return wildcard_pool
                .choose(&mut rand::thread_rng())
                .map(|t| (*t).clone());
        }
        matched
            .choose(&mut rand::thread_rng())
            .map(|t| (*t).clone())
    }

    /// Test-only: bypass the filesystem walk and seed the index directly.
    #[cfg(test)]
    pub async fn seed_for_test(&self, tracks: Vec<Track>) {
        *self.inner.write().await = tracks;
    }
}

/// Does `track`'s tag match any of `genres`? Case-insensitive, with
/// substring matching in both directions: persona genre `"jazz"` hits
/// track tag `"Soul Jazz"`, and persona genre `"new wave"` hits track
/// tag `"new-wave"`. Conservative enough that mistuned tags don't
/// leak across genres in `strict` mode.
fn track_matches_genres(track: &Track, genres: &[String]) -> bool {
    let Some(tag) = track.genre.as_deref() else {
        return false;
    };
    let tag_lower = tag.to_lowercase();
    genres.iter().any(|g| {
        let g_lower = g.to_lowercase();
        tag_lower.contains(&g_lower) || g_lower.contains(&tag_lower)
    })
}

fn walk(root: &Path) -> Vec<Track> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = match path.extension().and_then(|s| s.to_str()) {
            Some(e) => e,
            None => continue,
        };
        if !is_lossless(ext) {
            tracing::warn!(
                path = %path.display(),
                "skipping non-lossless file in library (Phase 1 is lossless-only)"
            );
            continue;
        }
        match read_tags(path, ext) {
            Some(track) => out.push(track),
            None => tracing::warn!(path = %path.display(), "could not read tags"),
        }
    }
    out
}

fn read_tags(path: &Path, ext: &str) -> Option<Track> {
    let tagged = Probe::open(path).ok()?.read().ok()?;
    let props = tagged.properties();
    let duration_ms = props.duration().as_millis() as u64;
    let sample_rate = props.sample_rate();
    let bit_depth = props.bit_depth();

    let (title, artist, album, year, genre) = if let Some(tag) = tagged.primary_tag() {
        (
            tag.title().map(|c| c.to_string()),
            tag.artist().map(|c| c.to_string()),
            tag.album().map(|c| c.to_string()),
            tag.year(),
            tag.genre().map(|c| c.to_string()),
        )
    } else {
        (None, None, None, None, None)
    };

    let fallback_title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .to_string();

    Some(Track {
        path: path.to_path_buf(),
        title: title.unwrap_or(fallback_title),
        artist: artist.unwrap_or_else(|| "Unknown Artist".into()),
        album: album.unwrap_or_else(|| "Unknown Album".into()),
        year,
        genre,
        label: None, // not exposed by lofty's primary tag API
        duration_ms,
        format: ext.to_ascii_lowercase(),
        sample_rate,
        bit_depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"").unwrap();
        p
    }

    #[tokio::test]
    async fn errors_when_root_missing() {
        let lib = MusicLibrary::new();
        let err = lib.scan("/this/does/not/exist/anywhere").await.unwrap_err();
        assert!(matches!(err, LibraryError::NotFound(_)));
    }

    #[tokio::test]
    async fn skips_lossy_files() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "track.mp3");
        touch(tmp.path(), "track.ogg");

        let lib = MusicLibrary::new();
        let count = lib.scan(tmp.path()).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn skips_extensionless_files() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "README");
        let lib = MusicLibrary::new();
        assert_eq!(lib.scan(tmp.path()).await.unwrap(), 0);
    }

    #[test]
    fn lossless_check() {
        assert!(is_lossless("flac"));
        assert!(is_lossless("FLAC"));
        assert!(is_lossless("wav"));
        assert!(is_lossless("aiff"));
        assert!(!is_lossless("mp3"));
        assert!(!is_lossless("m4a"));
    }

    #[tokio::test]
    async fn pick_random_returns_none_when_empty() {
        let lib = MusicLibrary::new();
        assert!(lib.pick_random().await.is_none());
    }

    fn fake_track(path: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            title: path.into(),
            artist: "x".into(),
            album: "x".into(),
            year: None,
            genre: None,
            label: None,
            duration_ms: 0,
            format: "flac".into(),
            sample_rate: None,
            bit_depth: None,
        }
    }

    fn fake_track_with_genre(path: &str, genre: &str) -> Track {
        let mut t = fake_track(path);
        t.genre = Some(genre.into());
        t
    }

    #[test]
    fn genre_matching_is_case_insensitive_and_bidirectional() {
        let t = fake_track_with_genre("/a", "Soul Jazz");
        assert!(track_matches_genres(&t, &["jazz".into()]));
        assert!(track_matches_genres(&t, &["JAZZ".into()]));
        assert!(track_matches_genres(&t, &["Soul".into()]));
        // Persona genre wider than tag — still matches via the other dir.
        let t = fake_track_with_genre("/a", "jazz");
        assert!(track_matches_genres(&t, &["soul jazz".into()]));
        // Total miss.
        assert!(!track_matches_genres(&t, &["techno".into()]));
        // Untagged tracks never match anything.
        let t = fake_track("/a");
        assert!(!track_matches_genres(&t, &["jazz".into()]));
    }

    #[tokio::test]
    async fn pick_strict_returns_only_matching_genre() {
        let lib = MusicLibrary::new();
        lib.seed_for_test(vec![
            fake_track_with_genre("/jazz.flac", "Jazz"),
            fake_track_with_genre("/synth.flac", "Synth-pop"),
            fake_track_with_genre("/bossa.flac", "Bossa Nova"),
        ])
        .await;
        let hist = super::super::history::TrackHistory::new(8);
        let genres = vec!["jazz".into(), "bossa".into()];
        // Drain a handful — should never see the synth-pop track.
        for _ in 0..40 {
            let pick = lib.pick(&hist, &genres, "strict").await.unwrap();
            assert!(
                pick.path != Path::new("/synth.flac"),
                "strict picked off-genre: {:?}",
                pick.path
            );
        }
    }

    #[tokio::test]
    async fn pick_strict_falls_back_when_no_matches() {
        let lib = MusicLibrary::new();
        lib.seed_for_test(vec![fake_track_with_genre("/only.flac", "Techno")])
            .await;
        let hist = super::super::history::TrackHistory::new(8);
        let genres = vec!["jazz".into()];
        // No jazz available — degrade to the wildcard pool rather than
        // returning None (silence is worse than off-genre).
        let pick = lib.pick(&hist, &genres, "strict").await.unwrap();
        assert_eq!(pick.path, PathBuf::from("/only.flac"));
    }

    #[tokio::test]
    async fn pick_free_ignores_genres() {
        let lib = MusicLibrary::new();
        lib.seed_for_test(vec![
            fake_track_with_genre("/jazz.flac", "Jazz"),
            fake_track_with_genre("/synth.flac", "Synth-pop"),
        ])
        .await;
        let hist = super::super::history::TrackHistory::new(8);
        let genres = vec!["jazz".into()];
        // free mode: both eligible. Verify across many picks both appear.
        let mut seen_synth = false;
        for _ in 0..200 {
            let pick = lib.pick(&hist, &genres, "free").await.unwrap();
            if pick.path == Path::new("/synth.flac") {
                seen_synth = true;
                break;
            }
        }
        assert!(seen_synth, "free mode should not filter on genre");
    }

    #[tokio::test]
    async fn pick_respects_history_within_genre() {
        let lib = MusicLibrary::new();
        lib.seed_for_test(vec![
            fake_track_with_genre("/a.flac", "Jazz"),
            fake_track_with_genre("/b.flac", "Jazz"),
        ])
        .await;
        let mut hist = super::super::history::TrackHistory::new(8);
        hist.record(PathBuf::from("/a.flac"), "x");
        let genres = vec!["jazz".into()];
        for _ in 0..15 {
            let pick = lib.pick(&hist, &genres, "strict").await.unwrap();
            assert_eq!(pick.path, PathBuf::from("/b.flac"));
        }
    }

    #[tokio::test]
    async fn pick_random_avoiding_skips_history() {
        let lib = MusicLibrary::new();
        lib.seed_for_test(vec![fake_track("/a.flac"), fake_track("/b.flac")])
            .await;
        let mut hist = super::super::history::TrackHistory::new(8);
        hist.record(PathBuf::from("/a.flac"), "x");
        // /a is in history AND its artist is too; /b shares the artist
        // but isn't path-blocked → cascade falls to path-fresh pool and
        // picks /b every time.
        for _ in 0..10 {
            let pick = lib.pick_random_avoiding(&hist).await.unwrap();
            assert_eq!(pick.path, PathBuf::from("/b.flac"));
        }
    }

    #[tokio::test]
    async fn pick_random_avoiding_falls_back_when_history_covers_library() {
        let lib = MusicLibrary::new();
        lib.seed_for_test(vec![fake_track("/a.flac")]).await;
        let mut hist = super::super::history::TrackHistory::new(8);
        hist.record(PathBuf::from("/a.flac"), "x");
        // Only track is in history → fallback path returns it anyway.
        let pick = lib.pick_random_avoiding(&hist).await.unwrap();
        assert_eq!(pick.path, PathBuf::from("/a.flac"));
    }

    fn artist_track(path: &str, artist: &str) -> Track {
        let mut t = fake_track(path);
        t.artist = artist.into();
        t
    }

    /// Two artists, two tracks each. After playing a Miles track, the
    /// next pick should be a Coltrane track even though three Miles
    /// tracks remain path-fresh — the artist ring spaces them out.
    #[tokio::test]
    async fn pick_random_avoiding_spaces_out_artists() {
        let lib = MusicLibrary::new();
        lib.seed_for_test(vec![
            artist_track("/m1.flac", "Miles Davis"),
            artist_track("/m2.flac", "Miles Davis"),
            artist_track("/c1.flac", "John Coltrane"),
            artist_track("/c2.flac", "John Coltrane"),
        ])
        .await;
        let mut hist = super::super::history::TrackHistory::new(8);
        hist.record(PathBuf::from("/m1.flac"), "Miles Davis");
        for _ in 0..10 {
            let pick = lib.pick_random_avoiding(&hist).await.unwrap();
            assert_eq!(
                pick.artist, "John Coltrane",
                "should prefer artist-fresh pool"
            );
        }
    }

    #[tokio::test]
    async fn pick_random_avoiding_empty_library_returns_none() {
        let lib = MusicLibrary::new();
        let hist = super::super::history::TrackHistory::new(8);
        assert!(lib.pick_random_avoiding(&hist).await.is_none());
    }
}
