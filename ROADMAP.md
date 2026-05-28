# Airtime — Phase 1 Roadmap

Personal AI radio station platform written in Rust. Multi-station Icecast2 streaming with LLM-scripted DJ segments, local TTS, and a self-hosted music library.

The original spec was written for Python; this port translates the architecture into idiomatic async Rust (tokio + reqwest + serde). External binaries (FFmpeg, Icecast2, Ollama, Kokoro) are invoked as subprocesses — same boundary as the Python design.

## Approach

- TDD where the boundary is purely in-process (config parsing, LLM router selection, skill scripting, scanner, feed parsing).
- Wiremock for HTTP-bound clients (Ollama, Claude, Open-Meteo, TomTom, RSS).
- Subprocess wrappers (FFmpeg, Kokoro, Icecast push) are thin and feature-gated behind runtime availability checks; their integration tests are skipped when the binary isn't on PATH.

## Phase 1 Deliverables

- [x] Cargo project skeleton + ROADMAP
- [x] Settings & persona TOML loader (`config`)
- [x] Music library scanner (lofty/symphonia) (`library`)
- [x] Feed fetchers — RSS news, Open-Meteo, TomTom (`feeds`)
- [x] Ollama LLM client (`llm::ollama`)
- [x] Claude API LLM client (`llm::claude`)
- [x] LLM router (per-skill backend selection) (`llm`)
- [x] Kokoro TTS subprocess wrapper (`tts`)
- [x] FFmpeg loudnorm normalizer (`audio::normalize`)
- [x] EQ profile processor — warm_analog, broadcast_crunch, telephone (`audio::eq`)
- [x] `Skill` trait + concrete skills:
  - [x] track_intro
  - [x] track_outro
  - [x] top_of_hour
  - [x] weather
  - [x] traffic
  - [x] station_id
  - [x] fake_ad (GTA mode)
  - [x] caller (GTA mode)
- [x] Segment scheduler (`scheduler`)
- [x] **Native Rust Icecast source client** — HTTP `PUT` + Basic auth, byte channel push (`stream`)
- [x] Segment sequencer / mixer (`mixer`)
- [x] Multi-station tokio runner (`main`)
- [x] Example personas (`personas/donna.toml`, `personas/mitch.toml`)
- [x] `settings.toml` global config

## Out of Scope (Phase 1)

Agentic SMS control, in-car Si4713 module, line-in passthrough, Cloudflare Tunnel sharing, royalty reporting, Web UI, secondary Kokoro instance for caller voice.

## Test Strategy

| Layer | Test type | Tool |
|---|---|---|
| Config (settings, persona) | Unit | `#[test]` on real TOML strings |
| LLM router | Unit | Trait double for `LlmBackend` |
| Ollama / Claude clients | Integration | `wiremock` |
| RSS / Open-Meteo / TomTom | Integration | `wiremock` |
| Library scanner | Integration | Fixture WAV file via `lofty` |
| Skills | Unit | Mock `LlmRouter` + `TtsEngine` + `AudioProcessor` |
| FFmpeg / Kokoro / Icecast push | Smoke (skipped if binary absent) | `Command` |
| Scheduler / mixer | Unit | Drive the queue manually |

## Notes

- Audio rendering is opaque blobs from the perspective of the orchestrator — the orchestrator only sees `PathBuf` + duration_ms.
- Each station owns one tokio task graph; stations are isolated from each other.
- The mixer pre-renders the next segment while the current one plays (queue depth ≥ 1).
- Feeds are cached and refreshed on independent schedules.
