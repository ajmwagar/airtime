# Airtime — Agentic Control & Persona System

**Version:** 0.1
**Status:** Draft
**Parent doc:** airtime-design-doc.md

-----

## 1. Agentic Control

Airtime has no dedicated control UI. The station is controlled entirely via natural language over SMS (or any messaging platform with a webhook). An AI agent interprets commands and maps them to Airtime's internal control API.

### 1.1 Architecture

```
User (SMS / iMessage / Signal)
 → OpenClaw webhook (or equivalent)
 → Claude agent (tool use mode)
 → Airtime Control API (local HTTP)
 → Scheduler / Mixer / Persona engine
 → Optional: confirmation reply via SMS
```

### 1.2 Messaging Gateway

**Primary:** OpenClaw or equivalent SMS webhook bridge
**Alternatives:** Twilio, Signal-cli, Apple Messages for Business

The gateway receives inbound messages, forwards to the agent endpoint, and returns the agent's reply as an SMS response.

### 1.3 Agent

- Model: Claude (tool use)
- System prompt includes: current station state, active persona, now-playing context, available tools
- Stateless per message — full context injected each call
- Responds conversationally, confirms actions taken

**Example exchange:**

```
User: "skip this, put on something more upbeat"
Agent: "Skipped. Queuing Stevie Wonder — Higher Ground (1974).
 Upbeat filter active for next 5 tracks."

User: "who produced this album?"
Agent: "That's Innervisions, produced by Stevie Wonder himself.
 Released August 1973 on Tamla."

User: "switch Donna to GTA mode"
Agent: "KFLT is now WDGN. Mitch Bravo is on the air.
 God help us all."
```

### 1.4 Control API (Local HTTP)

Airtime exposes a local REST API that the agent calls via tools. Not exposed to the public internet — loopback or LAN only.

| Endpoint            | Method | Description                              |
| ------------------- | ------ | ---------------------------------------- |
| `/track/skip`       | POST   | Skip current track                       |
| `/track/now`        | GET    | Now playing info                         |
| `/mode/set`         | POST   | Switch station mode                      |
| `/host/set`         | POST   | Swap active persona                      |
| `/genre/filter`     | POST   | Genre + duration filter                  |
| `/segment/queue`    | POST   | Force a segment type                     |
| `/segment/density`  | POST   | Adjust talk/music ratio                  |
| `/schedule/block`   | POST   | Override programming block               |

### 1.5 Agent Tools

```json
[
  { "name": "skip_track", "description": "Skip the current track" },
  { "name": "get_now_playing", "description": "Get current track and artist metadata" },
  { "name": "set_mode", "description": "Switch station mode (normal, gta, late_night, ambient)" },
  { "name": "set_host", "description": "Swap active host persona by name" },
  { "name": "set_genre_filter", "description": "Restrict music to genre for N tracks or minutes" },
  { "name": "queue_segment", "description": "Force a segment type: news, weather, traffic, fake_ad, caller" },
  { "name": "set_segment_density", "description": "Adjust talk/music ratio (light / normal / heavy)" },
  { "name": "set_schedule_block", "description": "Override current programming block" },
  { "name": "get_station_status", "description": "Return full station state: mode, host, queue, next segment" }
]
```

-----

## 2. Host Personality Files

Each station host is defined by a personality file — a structured document that controls LLM prompt behavior, TTS voice selection, and segment skill configuration.

### 2.1 File Format

TOML. One file per host. Stored in `/personas/`.

```toml
[host]
name = "Donna Wavelength"
callsign = "KFLT"
era = "1960s"
genre = ["jazz", "soul", "bossa nova"]
voice_model = "kokoro-voice-04"
tone_prompt = """
Warm, slightly sardonic late-night jazz host.
1960s AM radio energy. Knows every session musician
by name. Occasionally wistful. Never hurried.
"""

[host.skills]
track_intro = true
track_outro = true
top_of_hour = true
weather = true
traffic = false
station_id = true
fake_ad = false
caller = false
rant = false

[host.skill_config.top_of_hour]
max_words = 80
news_sources = ["npr", "ap"]
tone_override = "deliver headlines like you're telling a friend something sad but interesting"

[host.skill_config.weather]
max_words = 40
tone_override = "poetic, seasonal, never clinical"

[host.audio]
eq_profile = "warm_analog" # applies subtle vinyl-era EQ to TTS output
room_tone = true # adds light room ambience
loudness_target = -14 # LUFS
```

### 2.2 GTA Mode Persona

```toml
[host]
name = "Mitch Bravo"
callsign = "WDGN"
era = "1990s"
genre = ["classic rock", "hair metal"]
voice_model = "kokoro-voice-07"
tone_prompt = """
Unhinged drive-time host. Conspiracy-adjacent worldview.
Treats every traffic incident as a breaking warzone dispatch.
Fake sponsors for products that don't exist and shouldn't.
Caller segments with local 'characters.'
Never breaks character. Ever.
"""

[host.skills]
track_intro = true
track_outro = true
top_of_hour = true
weather = true
traffic = true
station_id = true
fake_ad = true
caller = true
rant = true

[host.skill_config.traffic]
max_words = 100
tone_override = "field reporter embedded in a traffic apocalypse. dramatize everything."

[host.skill_config.fake_ad]
max_words = 60
product_style = "absurdist_1990s"

[host.skill_config.caller]
voice_model = "kokoro-voice-11" # separate voice for caller character
eq_profile = "telephone" # bandpass 300Hz–3.4kHz, light sat
max_words = 50

[host.skill_config.rant]
max_words = 120
trigger = "top_of_hour" # when to fire unprompted
frequency = 0.3 # 30% chance per trigger window

[host.audio]
eq_profile = "broadcast_crunch"
room_tone = false
loudness_target = -14
```

-----

## 3. Segment Skills

Segments are modular, self-contained generation units. Each skill knows its own prompt structure, length constraints, data dependencies, and audio post-processing profile.

Skills are enabled/disabled per persona in the personality file.

### 3.1 Skill Interface

Every skill implements the same interface:

```
inputs: { station_context, host_persona, live_data?, track_metadata? }
process: fetch data → build prompt → LLM generate → TTS render → audio post
output: { audio_file: WAV, duration_ms: int, segment_type: string }
```

### 3.2 Skill Definitions

#### `track_intro`

- **Data:** track title, artist, year, label, genre, optional news color
- **LLM:** Ollama (local) — low latency, short copy
- **Length:** 20–50 words
- **Audio post:** match host EQ profile

#### `track_outro`

- **Data:** same as intro, optionally references what just played
- **LLM:** Ollama
- **Length:** 10–30 words

#### `top_of_hour`

- **Data:** RSS headlines (top 3), current time
- **LLM:** Claude API — summarization quality matters here
- **Length:** 60–100 words
- **Audio post:** slight level boost, cleaner EQ (news read energy)

#### `weather`

- **Data:** Open-Meteo current + 12hr forecast
- **LLM:** Ollama
- **Length:** 30–50 words
- **Tone:** driven by `tone_override` in persona file

#### `traffic`

- **Data:** TomTom / HERE incidents + travel times
- **LLM:** Ollama (normal) / Claude API (GTA mode)
- **Length:** 40–80 words

#### `station_id`

- **Data:** callsign, optional tagline from persona
- **LLM:** Ollama or static template
- **Length:** 10–20 words
- **Audio post:** reverb tail, jingle layer (optional)

#### `fake_ad` *(GTA mode)*

- **Data:** none — fully generative
- **LLM:** Claude API
- **Length:** 50–70 words
- **Style:** driven by `product_style` in skill config
- **Audio post:** slight compression, 1990s radio ad EQ

#### `caller` *(GTA mode)*

- **Data:** optional: recent news headline as conversation seed
- **LLM:** Claude API — generates both host and caller lines
- **Voices:** host voice + caller voice (separate Kokoro model)
- **Length:** 60–100 words total exchange
- **Audio post:** caller line gets telephone EQ profile

#### `rant` *(GTA mode)*

- **Data:** optional: recent headline as seed
- **LLM:** Claude API
- **Length:** 80–120 words
- **Trigger:** probabilistic, fires at `top_of_hour` or `station_id` slots

### 3.3 Skill Execution Order (Scheduler View)

```
[station_id] → [track_intro] → [track] → [track_outro]
→ [track_intro] → [track] → [track_outro]
→ ... (N tracks per block)
→ [top_of_hour] → [weather] → [traffic?]
→ [fake_ad?] → [caller?]
→ repeat
```

Exact cadence controlled by programming block and `segment_density` setting.

-----

## 4. Persona Switching

Switching host mid-stream via agent command:

1. Agent calls `set_host(name)`
2. Control API loads new persona TOML
3. Scheduler drains current segment queue (or cuts immediately, agent decides)
4. New persona's skills and voice activate on next segment
5. Optional: auto-generate a "handoff" `station_id` segment acknowledging the switch

-----

## 5. Adding a New Persona

1. Create `/personas/new-host.toml`
2. Enable/disable skills in `[host.skills]`
3. Set `voice_model` to available Kokoro voice
4. Write `tone_prompt` — this is the primary creative lever
5. Restart not required — agent can hot-load via `set_host()`
