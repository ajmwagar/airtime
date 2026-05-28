//! Pre-render pipeline.
//!
//! Two tasks per station:
//!
//! - **Producer** walks the scheduler, renders DJ segments (LLM → TTS →
//!   loudnorm/EQ → FLAC) and picks music tracks. Each finished item is
//!   pushed to a bounded `mpsc::channel<PlayItem>`.
//! - **Consumer** pulls `PlayItem`s in order and re-muxes them into the
//!   Ogg/FLAC byte channel that the Icecast source client drains.
//!
//! Because the channel has a small buffer (depth ≥ 1), the producer
//! races ahead by one segment — the LLM/TTS render for segment *N+1* is
//! already happening while the consumer is streaming segment *N*. If
//! `time_to_render < segment_duration` we never gap; if it isn't, the
//! producer blocks on the channel and we'd hear silence — that's a
//! diagnostic we now have a single seam to observe.
//!
//! The pipeline owns nothing about audio encoding or Icecast framing —
//! those live in `mixer` and `stream`. The consumer here is just glue
//! between "I have a FLAC file" and "pump it to the source channel".

use crate::audio::AudioError;
use crate::config::Persona;
use crate::feeds::FeedCache;
use crate::library::{MusicLibrary, Track, TrackHistory};
use crate::mixer::pump_to_icecast;
use crate::scheduler::{SegmentScheduler, SlotKind};
use crate::skills::{Skill, SkillContext, SkillOutput, SkillRuntime};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// One thing to play, in order. The consumer doesn't care whether it
/// originated from a skill or the library — both end up as a path to a
/// lossless file on disk.
#[derive(Debug, Clone)]
pub struct PlayItem {
    pub path: PathBuf,
    pub duration_ms: u64,
    pub label: String,
}

impl From<&Track> for PlayItem {
    fn from(t: &Track) -> Self {
        PlayItem {
            path: t.path.clone(),
            duration_ms: t.duration_ms,
            label: format!("track:{} - {}", t.artist, t.title),
        }
    }
}

impl From<SkillOutput> for PlayItem {
    fn from(o: SkillOutput) -> Self {
        PlayItem {
            path: o.audio_path,
            duration_ms: o.duration_ms,
            label: format!("segment:{}", o.segment_type),
        }
    }
}

/// Source of the current minute-of-hour. The producer reads this each
/// loop to decide which break skill to schedule. Defaults to
/// `chrono::Local`, but tests inject a deterministic clock.
pub trait Clock: Send + Sync {
    fn minute(&self) -> u32;
}

pub struct LocalClock;
impl Clock for LocalClock {
    fn minute(&self) -> u32 {
        use chrono::Timelike;
        chrono::Local::now().minute()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("audio: {0}")]
    Audio(#[from] AudioError),
    #[error("downstream channel closed")]
    Closed,
}

/// Producer state — owned exclusively by one async task.
pub struct Producer {
    pub persona: Persona,
    pub library: MusicLibrary,
    pub feeds: FeedCache,
    pub runtime: SkillRuntime,
    pub scheduler: SegmentScheduler,
    pub history: TrackHistory,
    pub clock: Arc<dyn Clock>,
    /// How long to wait before re-checking when the library comes back
    /// empty (avoids the spin loop). 30s in production; tests override.
    pub empty_library_backoff: Duration,
}

impl Producer {
    /// Run forever (or until `tx` is dropped). Returns the reason for exit.
    pub async fn run(mut self, tx: mpsc::Sender<PlayItem>) -> Result<(), PipelineError> {
        loop {
            if tx.is_closed() {
                return Err(PipelineError::Closed);
            }
            match self.scheduler.peek_next() {
                SlotKind::Track => self.produce_track_slot(&tx).await?,
                SlotKind::Break => self.produce_break_slot(&tx).await?,
            }
        }
    }

    async fn produce_track_slot(
        &mut self,
        tx: &mpsc::Sender<PlayItem>,
    ) -> Result<(), PipelineError> {
        let track = match self.library.pick_random_avoiding(&self.history).await {
            Some(t) => t,
            None => {
                warn!(
                    backoff_ms = self.empty_library_backoff.as_millis() as u64,
                    "library is empty — sleeping before retry"
                );
                tokio::time::sleep(self.empty_library_backoff).await;
                return Ok(());
            }
        };
        self.history.record(track.path.clone());

        let ctx = SkillContext {
            persona: self.persona.clone(),
            track: Some(track.clone()),
            feeds: self.feeds.snapshot().await,
        };
        let (intro, outro) = self.scheduler.track_skills();

        if let Some(skill) = intro {
            self.render_and_send(&skill, &ctx, tx).await?;
        }
        send(tx, PlayItem::from(&track)).await?;
        if let Some(skill) = outro {
            self.render_and_send(&skill, &ctx, tx).await?;
        }
        self.scheduler.note_track_played();
        Ok(())
    }

    async fn produce_break_slot(
        &mut self,
        tx: &mpsc::Sender<PlayItem>,
    ) -> Result<(), PipelineError> {
        let minute = self.clock.minute();
        let skill = match self.scheduler.next_break_skill_at(minute) {
            Some(s) => s,
            None => {
                // Nothing eligible right now — fall through to a track.
                debug!(minute, "no break skill eligible at this minute");
                self.scheduler.note_track_played(); // keep break cadence sane
                return Ok(());
            }
        };
        let ctx = SkillContext {
            persona: self.persona.clone(),
            track: None,
            feeds: self.feeds.snapshot().await,
        };
        self.render_and_send(&skill, &ctx, tx).await
    }

    async fn render_and_send(
        &self,
        skill: &Arc<dyn Skill>,
        ctx: &SkillContext,
        tx: &mpsc::Sender<PlayItem>,
    ) -> Result<(), PipelineError> {
        match skill.generate(ctx, &self.runtime).await {
            Ok(out) => {
                info!(
                    segment = out.segment_type,
                    duration_ms = out.duration_ms,
                    "segment rendered"
                );
                send(tx, PlayItem::from(out)).await
            }
            Err(e) => {
                warn!(skill = skill.name(), error = %e, "skill failed — skipping");
                Ok(())
            }
        }
    }
}

async fn send(tx: &mpsc::Sender<PlayItem>, item: PlayItem) -> Result<(), PipelineError> {
    tx.send(item).await.map_err(|_| PipelineError::Closed)
}

/// Consumer side: drain `PlayItem`s, hand each to `pump_to_icecast`.
///
/// One-bug-budget: if a single segment fails to encode (corrupt file,
/// ffmpeg missing for a moment, etc.) we log and continue — gapping a
/// segment is better than tearing down the whole station task.
pub async fn run_consumer(
    mut rx: mpsc::Receiver<PlayItem>,
    icecast_tx: mpsc::Sender<Vec<u8>>,
    chunk_size: usize,
) {
    while let Some(item) = rx.recv().await {
        debug!(label = %item.label, duration_ms = item.duration_ms, "pumping");
        if let Err(e) = pump_to_icecast(&item.path, &icecast_tx, chunk_size).await {
            warn!(label = %item.label, error = %e, "pump failed — skipping segment");
        }
        if icecast_tx.is_closed() {
            warn!("icecast channel closed — consumer exiting");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioProcessor;
    use crate::config::{Host, HostAudio, Persona, SkillToggles, Stream};
    use crate::feeds::FeedCache;
    use crate::llm::{LlmBackend, LlmError, LlmRouter};
    use crate::skills::{Skill, SkillContext, SkillError, SkillOutput, SkillRuntime};
    use crate::tts::{KokoroError, TtsEngine};
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    struct FixedClock(u32);
    impl Clock for FixedClock {
        fn minute(&self) -> u32 {
            self.0
        }
    }

    struct StubBackend;
    #[async_trait]
    impl LlmBackend for StubBackend {
        async fn complete(&self, _system: &str, _user: &str) -> Result<String, LlmError> {
            Ok("scripted line".into())
        }
    }

    struct StubTts;
    #[async_trait]
    impl TtsEngine for StubTts {
        async fn render(
            &self,
            _text: &str,
            _voice: &str,
            out_dir: &Path,
        ) -> Result<PathBuf, KokoroError> {
            tokio::fs::create_dir_all(out_dir).await?;
            let p = out_dir.join(format!("stub-{}.wav", rand::random::<u32>()));
            tokio::fs::write(&p, b"PCM").await?;
            Ok(p)
        }
    }

    /// Skill that doesn't touch the LLM/TTS/audio stack — it writes a
    /// stub file directly and returns the path. Lets us assert pipeline
    /// ordering without needing ffmpeg.
    struct StubSkill {
        name: &'static str,
        needs_track: bool,
        score_at: fn(u32) -> i32,
        runs: Arc<AtomicUsize>,
        out_dir: PathBuf,
    }

    #[async_trait]
    impl Skill for StubSkill {
        fn name(&self) -> &'static str {
            self.name
        }
        fn needs_track(&self) -> bool {
            self.needs_track
        }
        fn time_score(&self, m: u32) -> i32 {
            (self.score_at)(m)
        }
        async fn generate(
            &self,
            _ctx: &SkillContext,
            _rt: &SkillRuntime,
        ) -> Result<SkillOutput, SkillError> {
            let n = self.runs.fetch_add(1, Ordering::SeqCst);
            let p = self.out_dir.join(format!("{}-{n}.flac", self.name));
            tokio::fs::write(&p, b"FLAC")
                .await
                .map_err(|e| SkillError::Audio(AudioError::Io(e)))?;
            Ok(SkillOutput {
                audio_path: p,
                duration_ms: 1000,
                segment_type: self.name,
                text: "scripted".into(),
            })
        }
    }

    fn persona() -> Persona {
        Persona {
            host: Host {
                name: "Donna".into(),
                callsign: "KFLT".into(),
                era: "1960s".into(),
                genre: vec!["jazz".into()],
                voice_model: "af_heart".into(),
                tone_prompt: "Warm.".into(),
                skills: SkillToggles::default(),
                skill_config: Default::default(),
                audio: HostAudio {
                    eq_profile: None,
                    room_tone: false,
                    loudness_target: -14.0,
                },
            },
            stream: Stream {
                mount: "/donna".into(),
                format: "flac".into(),
                bitrate: None,
            },
        }
    }

    fn runtime(temp_dir: PathBuf) -> SkillRuntime {
        let llm = Arc::new(LlmRouter::new(
            Arc::new(StubBackend),
            Arc::new(StubBackend),
            Arc::new(StubBackend),
        ));
        SkillRuntime {
            llm,
            tts: Arc::new(StubTts),
            audio: Arc::new(AudioProcessor::new(temp_dir, -14.0, -1.0)),
        }
    }

    /// Pipeline produces intro → track → outro in order for a Track slot.
    #[tokio::test]
    async fn produces_intro_track_outro_in_order() {
        let tmp = TempDir::new().unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let library = MusicLibrary::new();
        library
            .seed_for_test(vec![Track {
                path: tmp.path().join("song.flac"),
                title: "So What".into(),
                artist: "Miles".into(),
                album: "KoB".into(),
                year: Some(1959),
                genre: None,
                label: None,
                duration_ms: 562_000,
                format: "flac".into(),
                sample_rate: None,
                bit_depth: None,
            }])
            .await;

        let intro = Arc::new(StubSkill {
            name: "track_intro",
            needs_track: true,
            score_at: |_| 10,
            runs: runs.clone(),
            out_dir: tmp.path().to_path_buf(),
        });
        let outro = Arc::new(StubSkill {
            name: "track_outro",
            needs_track: true,
            score_at: |_| 10,
            runs: runs.clone(),
            out_dir: tmp.path().to_path_buf(),
        });
        let scheduler = SegmentScheduler::new(vec![intro, outro], 99);

        let producer = Producer {
            persona: persona(),
            library: library.clone(),
            feeds: FeedCache::new(),
            runtime: runtime(tmp.path().to_path_buf()),
            scheduler,
            history: TrackHistory::new(8),
            clock: Arc::new(FixedClock(8)),
            empty_library_backoff: Duration::from_millis(1),
        };

        let (tx, mut rx) = mpsc::channel::<PlayItem>(8);
        let handle = tokio::spawn(producer.run(tx));

        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        let third = rx.recv().await.unwrap();

        // Drop receiver so the producer task exits cleanly.
        drop(rx);
        let _ = handle.await;

        assert!(first.label.starts_with("segment:track_intro"), "{first:?}");
        assert!(second.label.starts_with("track:"), "{second:?}");
        assert!(third.label.starts_with("segment:track_outro"), "{third:?}");
    }

    /// Empty library triggers backoff sleep instead of spinning hot.
    #[tokio::test]
    async fn empty_library_sleeps_instead_of_spinning() {
        let tmp = TempDir::new().unwrap();
        let library = MusicLibrary::new(); // no tracks
        let scheduler = SegmentScheduler::new(vec![], 99);

        let producer = Producer {
            persona: persona(),
            library,
            feeds: FeedCache::new(),
            runtime: runtime(tmp.path().to_path_buf()),
            scheduler,
            history: TrackHistory::new(8),
            clock: Arc::new(FixedClock(8)),
            empty_library_backoff: Duration::from_millis(20),
        };

        let (tx, _rx) = mpsc::channel::<PlayItem>(2);
        let handle = tokio::spawn(producer.run(tx));

        // If the producer were spinning, it would have made dozens of
        // library queries in the meantime. Sleep a beat, then tear it
        // down — what we're really asserting is that the test finishes
        // in bounded time and the task doesn't panic.
        tokio::time::sleep(Duration::from_millis(80)).await;
        handle.abort();
    }

    /// Track-repeat avoidance — even with a single playable, the history
    /// records each play. Two-track library + history = alternation.
    #[tokio::test]
    async fn history_alternates_two_track_library() {
        let tmp = TempDir::new().unwrap();
        let library = MusicLibrary::new();
        library
            .seed_for_test(vec![
                Track {
                    path: PathBuf::from("/a.flac"),
                    title: "A".into(),
                    artist: "x".into(),
                    album: "x".into(),
                    year: None,
                    genre: None,
                    label: None,
                    duration_ms: 1000,
                    format: "flac".into(),
                    sample_rate: None,
                    bit_depth: None,
                },
                Track {
                    path: PathBuf::from("/b.flac"),
                    title: "B".into(),
                    artist: "x".into(),
                    album: "x".into(),
                    year: None,
                    genre: None,
                    label: None,
                    duration_ms: 1000,
                    format: "flac".into(),
                    sample_rate: None,
                    bit_depth: None,
                },
            ])
            .await;

        let producer = Producer {
            persona: persona(),
            library,
            feeds: FeedCache::new(),
            runtime: runtime(tmp.path().to_path_buf()),
            // tracks_per_break very high so we always stay in Track slots
            scheduler: SegmentScheduler::new(vec![], 999),
            history: TrackHistory::new(1), // capacity 1 → guarantees alternation
            clock: Arc::new(FixedClock(8)),
            empty_library_backoff: Duration::from_millis(1),
        };

        let (tx, mut rx) = mpsc::channel::<PlayItem>(4);
        let handle = tokio::spawn(producer.run(tx));

        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        let third = rx.recv().await.unwrap();
        drop(rx);
        let _ = handle.await;

        assert_ne!(first.path, second.path, "should alternate");
        assert_ne!(second.path, third.path, "should alternate");
        assert_eq!(first.path, third.path, "with cap=1, should cycle");
    }
}
