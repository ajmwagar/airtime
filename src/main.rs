//! Airtime entry point — boot every persona under `personas/` as an
//! independent station task.

use airtime::audio::AudioProcessor;
use airtime::config::{Persona, Settings};
use airtime::feeds::{news::NewsFetcher, weather::WeatherFetcher, FeedCache};
use airtime::library::MusicLibrary;
use airtime::llm::{claude::ClaudeClient, ollama::OllamaClient, LlmRouter};
use airtime::mixer::pump_to_icecast;
use airtime::scheduler::{SegmentScheduler, SlotKind};
use airtime::skills::{build_enabled, SkillContext, SkillRuntime};
use airtime::stream::{run_source, SourceConfig};
use airtime::tts::KokoroTts;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings_path = std::env::var("AIRTIME_SETTINGS").unwrap_or_else(|_| "settings.toml".into());
    let settings = Arc::new(Settings::load(&settings_path).with_context(|| {
        format!("loading settings from {settings_path}")
    })?);
    info!(
        path = %settings_path,
        "settings loaded"
    );

    // Build the shared LLM router + TTS engine + audio processor.
    let ollama = Arc::new(OllamaClient::new(&settings.ollama.base_url, &settings.ollama.model));
    let claude: Arc<dyn airtime::llm::LlmBackend> = match ClaudeClient::from_env(&settings.claude.model) {
        Ok(c) => Arc::new(c),
        Err(_) => {
            warn!("ANTHROPIC_API_KEY not set — claude backend will return errors");
            Arc::new(NoopBackend("ANTHROPIC_API_KEY missing"))
        }
    };
    let llm = Arc::new(LlmRouter::new(ollama, claude));

    let tts_bin = settings
        .kokoro
        .binary
        .clone()
        .or_else(|| std::env::var("KOKORO_BIN").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("kokoro"));
    let tts: Arc<dyn airtime::tts::TtsEngine> = Arc::new(KokoroTts::new(
        tts_bin,
        settings.kokoro.model_path.clone(),
        settings.kokoro.voices_path.clone(),
    ));

    let audio = Arc::new(AudioProcessor::new(
        settings.audio.temp_dir.clone(),
        settings.audio.loudness_target,
        settings.audio.true_peak,
    ));

    let runtime = SkillRuntime {
        llm,
        tts,
        audio: audio.clone(),
    };

    // Music library.
    let library = MusicLibrary::new();
    if settings.library.scan_on_start {
        match library.scan(&settings.library.path).await {
            Ok(n) => info!(tracks = n, path = %settings.library.path.display(), "library scanned"),
            Err(e) => warn!(error = %e, "library scan failed"),
        }
    }

    // Shared feed cache + refreshers.
    let feeds = FeedCache::new();
    spawn_feed_refreshers(feeds.clone(), settings.clone());

    // Boot one task per persona.
    let personas_dir = std::env::var("AIRTIME_PERSONAS").unwrap_or_else(|_| "personas".into());
    let mut handles = Vec::new();
    let mut found = 0;
    let mut entries = tokio::fs::read_dir(&personas_dir)
        .await
        .with_context(|| format!("reading personas dir {personas_dir}"))?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let persona = match Persona::load(&path) {
            Ok(p) => p,
            Err(e) => {
                error!(path = %path.display(), error = %e, "failed to load persona");
                continue;
            }
        };
        found += 1;
        let st = settings.clone();
        let rt = runtime.clone();
        let lib = library.clone();
        let fd = feeds.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = run_station(persona, st, rt, lib, fd).await {
                error!(error = %e, "station task exited");
            }
        }));
    }

    if found == 0 {
        warn!(dir = %personas_dir, "no persona .toml files found");
    }

    futures::future::join_all(handles).await;
    Ok(())
}

struct NoopBackend(&'static str);
#[async_trait::async_trait]
impl airtime::llm::LlmBackend for NoopBackend {
    async fn complete(
        &self,
        _system: &str,
        _user: &str,
    ) -> Result<String, airtime::llm::LlmError> {
        Err(airtime::llm::LlmError::Malformed(self.0.into()))
    }
}

fn spawn_feed_refreshers(feeds: FeedCache, settings: Arc<Settings>) {
    // News: collected per-persona at the persona's configured sources;
    // here we just keep the cache fresh from a default empty list. The
    // station task can refresh its own.
    let news_feeds = feeds.clone();
    tokio::spawn(async move {
        let fetcher = NewsFetcher::new();
        loop {
            // No global sources — station tasks may push their own news.
            // Refresh interval is still respected for backoff.
            let _ = &fetcher;
            let _ = &news_feeds;
            tokio::time::sleep(Duration::from_secs(60 * 60)).await;
        }
    });

    // Weather.
    let w_feeds = feeds.clone();
    let st = settings.clone();
    tokio::spawn(async move {
        let fetcher = WeatherFetcher::new();
        loop {
            match fetcher
                .fetch(st.feeds.weather_lat, st.feeds.weather_lon)
                .await
            {
                Ok(w) => w_feeds.set_weather(w).await,
                Err(e) => warn!(error = %e, "weather fetch failed"),
            }
            tokio::time::sleep(Duration::from_secs(60 * 30)).await;
        }
    });

    // Traffic.
    if settings.feeds.traffic.is_some() {
        if let Ok(fetcher) = airtime::feeds::traffic::TrafficFetcher::from_env() {
            let t_feeds = feeds.clone();
            let st = settings.clone();
            tokio::spawn(async move {
                loop {
                    let bbox = (
                        st.feeds.weather_lon - 0.3,
                        st.feeds.weather_lat - 0.3,
                        st.feeds.weather_lon + 0.3,
                        st.feeds.weather_lat + 0.3,
                    );
                    match fetcher.fetch(bbox).await {
                        Ok(t) => t_feeds.set_traffic(t).await,
                        Err(e) => warn!(error = %e, "traffic fetch failed"),
                    }
                    tokio::time::sleep(Duration::from_secs(60 * 15)).await;
                }
            });
        } else {
            warn!("traffic provider configured but TOMTOM_API_KEY missing");
        }
    }
}

async fn run_station(
    persona: Persona,
    settings: Arc<Settings>,
    rt: SkillRuntime,
    library: MusicLibrary,
    feeds: FeedCache,
) -> Result<()> {
    info!(callsign = %persona.host.callsign, mount = %persona.stream.mount, "station starting");

    let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(64);
    let cfg = SourceConfig {
        host: settings.icecast.host.clone(),
        port: settings.icecast.port,
        user: settings.icecast.user.clone(),
        password: settings.icecast.password.clone(),
        mount: persona.stream.mount.clone(),
        content_type: "application/ogg".into(),
        station_name: persona.host.callsign.clone(),
        genre: persona.host.genre.join(", "),
        description: persona.host.tone_prompt.lines().next().unwrap_or("").into(),
    };

    let src_task = tokio::spawn(async move {
        if let Err(e) = run_source(cfg, audio_rx).await {
            error!(error = %e, "icecast source dropped");
        }
    });

    let skills = build_enabled(&persona.host.skills);
    let mut scheduler = SegmentScheduler::new(skills, 3);

    loop {
        let slot = scheduler.peek_next();
        let ctx = SkillContext {
            persona: persona.clone(),
            track: library.pick_random().await,
            feeds: feeds.snapshot().await,
        };
        match slot {
            SlotKind::Track => {
                let (intro, outro) = scheduler.track_skills();
                if let Some(intro) = intro {
                    if ctx.track.is_some() {
                        render_and_push(&intro, &ctx, &rt, &audio_tx).await;
                    }
                }
                if let Some(track) = ctx.track.as_ref() {
                    if let Err(e) = pump_to_icecast(&track.path, &audio_tx, 16 * 1024).await {
                        warn!(error = %e, "failed to stream track");
                    }
                }
                if let Some(outro) = outro {
                    if ctx.track.is_some() {
                        render_and_push(&outro, &ctx, &rt, &audio_tx).await;
                    }
                }
                scheduler.note_track_played();
            }
            SlotKind::Break => {
                if let Some(skill) = scheduler.next_break_skill() {
                    render_and_push(&skill, &ctx, &rt, &audio_tx).await;
                }
            }
        }

        if audio_tx.is_closed() {
            warn!("icecast channel closed — exiting station loop");
            break;
        }
    }
    src_task.abort();
    Ok(())
}

async fn render_and_push(
    skill: &Arc<dyn airtime::skills::Skill>,
    ctx: &SkillContext,
    rt: &SkillRuntime,
    audio_tx: &mpsc::Sender<Vec<u8>>,
) {
    match skill.generate(ctx, rt).await {
        Ok(output) => {
            info!(
                segment = output.segment_type,
                duration_ms = output.duration_ms,
                "segment rendered"
            );
            if let Err(e) = pump_to_icecast(&output.audio_path, audio_tx, 16 * 1024).await {
                warn!(error = %e, "failed to stream segment");
            }
        }
        Err(e) => warn!(skill = skill.name(), error = %e, "skill failed"),
    }
}
