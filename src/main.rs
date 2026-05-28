//! Airtime entry point — boot every persona under `personas/` as an
//! independent station task.

use airtime::audio::AudioProcessor;
use airtime::config::{Persona, Settings};
use airtime::feeds::{news::NewsFetcher, weather::WeatherFetcher, FeedCache};
use airtime::library::{MusicLibrary, TrackHistory};
use airtime::llm::{
    claude::ClaudeClient, ollama::OllamaClient, openrouter::OpenRouterClient, LlmRouter,
};
use airtime::pipeline::{
    run_consumer, ConsumerConfig, KeepaliveConfig, LocalClock, PlayItem, Producer,
};
use airtime::scheduler::SegmentScheduler;
use airtime::skills::{build_enabled, SkillRuntime};
use airtime::stream::{run_source_with_retry, RetryPolicy, SourceConfig};
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

    let settings_path =
        std::env::var("AIRTIME_SETTINGS").unwrap_or_else(|_| "settings.toml".into());
    let settings = Arc::new(
        Settings::load(&settings_path)
            .with_context(|| format!("loading settings from {settings_path}"))?,
    );
    info!(
        path = %settings_path,
        "settings loaded"
    );

    // Build the shared LLM router + TTS engine + audio processor.
    let ollama = Arc::new(OllamaClient::new(
        &settings.ollama.base_url,
        &settings.ollama.model,
    ));
    let claude: Arc<dyn airtime::llm::LlmBackend> =
        match ClaudeClient::from_env(&settings.claude.model) {
            Ok(c) => Arc::new(c),
            Err(_) => {
                warn!("ANTHROPIC_API_KEY not set — claude backend will return errors");
                Arc::new(NoopBackend("ANTHROPIC_API_KEY missing"))
            }
        };
    let openrouter: Arc<dyn airtime::llm::LlmBackend> = match settings.openrouter.as_ref() {
        Some(or_cfg) => match std::env::var("OPENROUTER_API_KEY") {
            Ok(key) => {
                let model = or_cfg.model.clone();
                Arc::new(match or_cfg.base_url.as_deref() {
                    Some(url) => OpenRouterClient::with_base_url(url, model, key),
                    None => OpenRouterClient::new(model, key),
                })
            }
            Err(_) => {
                warn!("OPENROUTER_API_KEY not set — openrouter backend will return errors");
                Arc::new(NoopBackend("OPENROUTER_API_KEY missing"))
            }
        },
        None => Arc::new(NoopBackend("[openrouter] block not configured")),
    };
    let llm = Arc::new(LlmRouter::new(ollama, claude, openrouter));

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
    async fn complete(&self, _system: &str, _user: &str) -> Result<String, airtime::llm::LlmError> {
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
    info!(
        callsign = %persona.host.callsign,
        mount = %persona.stream.mount,
        "station starting"
    );

    // Three channels make up the audio chain:
    //   [Producer] --PlayItem--> [Consumer] --raw bytes--> [Icecast source]
    //
    // The PlayItem channel is bounded at depth 2 — enough for the
    // producer to keep one segment ahead of the consumer (the spec's
    // "queue depth ≥ 1") without unbounded memory growth if the network
    // back-pressures.
    let (item_tx, item_rx) = mpsc::channel::<PlayItem>(2);
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
        if let Err(e) = run_source_with_retry(cfg, audio_rx, RetryPolicy::default()).await {
            // Only fires on permanent config errors (bad mount, lossy
            // content-type). Network drops are retried internally.
            error!(error = %e, "icecast source permanently failed");
        }
    });

    // Pre-render the silence keepalive file. If ffmpeg isn't reachable
    // (sandbox / misconfig) we silently disable keepalive — the
    // consumer still works, just without the source-timeout safety net.
    let keepalive = match rt.audio.ensure_silence(3).await {
        Ok(path) => Some(KeepaliveConfig {
            silence_path: path,
            idle_after: Duration::from_secs(5),
        }),
        Err(e) => {
            warn!(error = %e, "could not pre-render silence keepalive; disabling");
            None
        }
    };
    let consumer_audio_tx = audio_tx.clone();
    let consumer_task = tokio::spawn(async move {
        run_consumer(
            item_rx,
            consumer_audio_tx,
            ConsumerConfig {
                chunk_size: 16 * 1024,
                keepalive,
            },
        )
        .await;
    });

    let scheduler = SegmentScheduler::new(build_enabled(&persona.host.skills), 3);
    let producer = Producer {
        persona,
        library,
        feeds,
        runtime: rt,
        scheduler,
        history: TrackHistory::default(),
        clock: Arc::new(LocalClock),
        empty_library_backoff: Duration::from_secs(30),
    };

    let producer_result = producer.run(item_tx).await;
    if let Err(e) = producer_result {
        warn!(error = %e, "producer exited");
    }

    drop(audio_tx);
    let _ = consumer_task.await;
    src_task.abort();
    Ok(())
}
