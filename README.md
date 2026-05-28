# Airtime — Technical Design Document

**Version:** 0.1
**Status:** Draft
**Scope:** Personal / Social Tier (Phase 1)

-----

## 1. Overview

Airtime is an AI-powered personal radio station platform with a golden-age broadcast aesthetic. It combines a self-hosted music library, LLM-generated DJ scripts, TTS voice synthesis, and live data feeds to produce a continuous, HiFi audio stream with authentic radio programming structure.

-----

## 2. Core Architecture

```
┌─────────────────────────────────────────────────────────┐
│ SCHEDULER                                               │
│ (time-of-day, segment queue, clock logic)               │
└────────────────────┬────────────────────────────────────┘
                     │
        ┌────────────┴────────────┐
        ▼                         ▼
┌───────────────┐       ┌─────────────────┐
│ DATA FEEDS    │       │ MUSIC ENGINE    │
│ - RSS/News    │       │ - Library scan  │
│ - Weather     │       │ - Metadata      │
│ - Traffic     │       │ - Gapless play  │
└──────┬────────┘       └────────┬────────┘
       │                         │
       ▼                         ▼
┌─────────────────────────────────────────┐
│ LLM SCRIPT ENGINE                       │
│ (Claude API / Ollama — persona-aware)   │
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ TTS RENDER LAYER                        │
│ (Kokoro — local)                        │
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ STREAM MIXER / OUTPUT                   │
│ FLAC → AirPlay 2 / Chromecast Audio     │
└─────────────────────────────────────────┘
```

-----

## 3. Subsystems

### 3.1 Scheduler

The scheduler is the clock backbone of Airtime. It owns the segment queue and fires triggers based on time-of-day programming blocks.

**Programming blocks:**

- Morning Show (6–10 AM) — higher news/weather/traffic density
- Midday (10 AM–2 PM) — music-heavy, lighter talk
- Drive Time (2–7 PM) — traffic-forward, news breaks
- Evening (7–11 PM) — mood-aware, low interruption
- Late Night (11 PM–6 AM) — minimal breaks, ambient-leaning

**Segment types (queue primitives):**

- `track` — music playback from library
- `intro` — DJ track intro, pre-rendered TTS
- `outro` — DJ track outro
- `top_of_hour` — news + weather break
- `traffic` — traffic segment (drive time only)
- `station_id` — callsign/jingle
- `fake_ad` — GTA mode only
- `caller` — AI caller segment, GTA mode only

**Pre-render window:** Segments render 60–120 seconds ahead of playback. Latency is acceptable since nothing is live by default.

-----

### 3.2 Music Engine

**Format support:**

- FLAC (preferred, lossless)
- WAV / AIFF
- MP3 / AAC (fallback, clearly marked in UI as lossy)

**Library indexing:**

- Beets for library management and MusicBrainz metadata lookup
- Fallback: embedded tags (ID3 / Vorbis comments)
- Fields used by LLM: `artist`, `album`, `year`, `genre`, `label`, `country`

**Playback:**

- Gapless playback required
- ReplayGain normalization (track + album mode) to level-match music vs. TTS segments
- Target loudness: –14 LUFS (streaming standard)

**Hybrid live input (Phase 2):**

- Line-in passthrough for turntable or live guest
- Ducking logic: auto-lower stream when live input detected above threshold
- Manual override via control surface or web UI

-----

### 3.3 Data Feeds

| Feed           | Source                  | Notes                               |
| -------------- | ----------------------- | ----------------------------------- |
| News           | RSS (user-configurable) | Default: NPR, AP, local news        |
| Weather        | Open-Meteo API          | Free, no key required               |
| Traffic        | TomTom or HERE API      | Free tier sufficient for personal use |
| Music metadata | MusicBrainz / Beets     | Local lookup, no rate limits        |

**Feed pipeline:**

1. Fetch raw data on schedule (news: hourly, weather: 30 min, traffic: 15 min)
2. Cache locally
3. Pass to LLM script engine at segment trigger time

-----

### 3.4 LLM Script Engine

**Dual-model architecture:**

| Task                  | Model                               | Rationale                                           |
| --------------------- | ----------------------------------- | --------------------------------------------------- |
| Track intros/outros   | Ollama (Llama 3.1 8B or Mistral 7B) | Low latency, runs local, good enough for short copy |
| News summarization    | Claude API (claude-sonnet)          | Better prose quality, handles complex headlines     |
| Weather / traffic copy| Ollama                              | Simple data-to-speech conversion                    |
| GTA mode scripts      | Claude API                          | Needs stronger creative/satirical capability        |
| Fake ads / caller bits| Claude API                          | Same                                                |

**Prompt structure per segment:**

```
System (untrusted):
You are {host.name}, a radio DJ on {station.callsign}.
Tone: {host.tone_prompt}
Era/genre context: {station.genre} radio, {station.era}
Output: spoken radio copy only. No stage directions. Max {segment.max_words} words.

USER:
Next track: "{track.title}" by {track.artist} ({track.year}, {track.label})
[optional: recent news headline for color]
Write a {segment_type} for this track.
```

**Persona schema:**

```json
{
  "host": {
    "name": "Donna Wavelength",
    "callsign": "KFLT",
    "tone_prompt": "Warm, slightly sardonic late-night jazz host. 1960s AM radio energy.",
    "voice_model": "kokoro-voice-04",
    "era": "1960s",
    "genre": "jazz",
    "ad_style": null
  }
}
```

**GTA mode extension:**

```json
{
  "host": {
    "name": "Mitch Bravo",
    "callsign": "WDGN",
    "tone_prompt": "Unhinged drive-time host. Conspiracy-adjacent. Fake sponsor reads. Treats traffic reports as breaking news from a war zone.",
    "voice_model": "kokoro-voice-07",
    "era": "1990s",
    "genre": "classic rock",
    "ad_style": "satirical_product",
    "caller_segments": true
  }
}
```

-----

### 3.5 TTS Render Layer

**Primary engine:** Kokoro (local, Apache 2.0)

- Multiple voice models, selectable per host persona
- Output: WAV, 24-bit/44.1kHz minimum
- Post-processing: light room tone / mild vinyl EQ optional per station theme
- Normalize to –14 LUFS before queueing

**Processing chain:**

```
LLM script (text)
 → Kokoro TTS render (WAV)
 → Optional EQ / room tone (FFmpeg)
 → ReplayGain normalization
 → Segment cache (queued for playback)
```

-----

### 3.6 Stream Mixer / Output

**Local network output:**

- FLAC stream via HTTP (icecast-compatible)
- No lossy transcoding in the signal chain

**Device support:**

- AirPlay 2
- Chromecast Audio
- DLNA / UPnP

**Mixer responsibilities:**

- Crossfade music → TTS segments (short fade, ~0.5s)
- Duck music under intros if overlap
- Insert station ID jingles on schedule

-----

## 4. HiFi Constraints

- No lossy transcoding anywhere in the internal chain
- Music source files: FLAC / WAV only for "HiFi mode" designation
- TTS output: 24-bit WAV minimum before normalization
- Output stream: FLAC or lossless PCM to endpoint
- ReplayGain applied to all segments before mixing
- Target: –14 LUFS / –1 dBTP true peak

-----

## 5. GTA Mode

An optional station persona mode inspired by GTA radio stations. Activates when `host.ad_style` and `host.caller_segments` are set.

**Additional segment types:**

- `fake_ad` — LLM-generated product spot, absurdist, era-appropriate
- `caller` — AI-generated "listener call-in," scripted, voice-acted via Kokoro with alternate voice
- `rant` — unprompted host monologue triggered on schedule

**Real data → satirized:**

- Actual headlines fed to Claude with satirize instruction
- Real traffic data reframed as dramatic field reports
- Weather delivered as ominous prophecy

**Caller voice:**

- Separate Kokoro voice model per station (not host voice)
- Phone EQ applied (bandpass ~300Hz–3.4kHz, light distortion) to sell the bit

-----

## 6. Phase Roadmap

| Phase | Scope                                                                         |
| ----- | ----------------------------------------------------------------------------- |
| 1     | Core scheduler, music engine, Ollama script gen, Kokoro TTS, local FLAC stream |
| 2     | Claude API integration for news/GTA mode, live line-in passthrough, AirPlay 2 |
| 3     | Social layer — shareable station URLs, listen-together sync                   |
| 4     | Creator tier — public stations, custom personas, hosted library               |

-----

## 7. Stack Summary

| Component              | Technology                          |
| ---------------------- | ----------------------------------- |
| Scheduler              | Rust or Go (latency-sensitive)      |
| Music engine / playback| GStreamer or Symphonia (Rust)       |
| Library indexing       | Beets + MusicBrainz                 |
| LLM (local)            | Ollama — Llama 3.1 8B / Mistral 7B  |
| LLM (cloud)            | Anthropic Claude API (claude-sonnet)|
| TTS                    | Kokoro (local, Apache 2.0)          |
| Audio post-processing  | FFmpeg                              |
| Loudness normalization | FFmpeg (loudnorm filter)            |
| Stream output          | Icecast2-compatible HTTP            |
| Device endpoints       | AirPlay 2, Chromecast Audio, DLNA   |
| News feeds             | RSS via custom fetcher              |
| Weather                | Open-Meteo API                      |
| Traffic                | TomTom or HERE API                  |
| Config / persona schema| JSON / TOML                         |
| Frontend (control UI)  | TBD — web-based                     |
