# Content & Cast Spec

**Status:** Draft / discussion
**Goal:** Decouple **who voices a segment** from **what they say** from **when it fires**, so a station can sound like a production (host + news guy + commercial actor + caller) instead of one person reading every script.

## Today's problem

A persona TOML mixes three things into "skills":

```toml
[host]
voice_model = "af_heart"             # 1 voice for everything

[host.skills]
top_of_hour = true                   # trigger
fake_ad     = true

[host.skill_config.top_of_hour]
news_sources = [...]                 # content source
llm_backend  = "claude"              # delivery LLM
```

Every enabled skill renders with `host.voice_model`. The news, the fake ad, the call-in caller — all sound like Donna. Skills are also tied to one fixed prompt shape baked into `src/skills/<name>.rs`.

## Proposed model — three orthogonal concepts

### 1. Cast — voices a station can use

A station has one host and zero-or-more supporting cast members. Each cast member has a voice, a tone prompt, and optionally its own LLM backend.

```toml
[host]
name        = "Donna Wavelength"
voice_model = "af_heart"
tone_prompt = "Warm, sardonic late-night jazz host."

[cast.news_anchor]
name        = "Walter Drake"
voice_model = "am_michael"
tone_prompt = "1960s evening-news baritone, authoritative, never editorial."
llm_backend = "claude"

[cast.commercial_actor]
voice_model = "af_sarah"
tone_prompt = "Enthusiastic 1960s spokesperson, mid-Atlantic accent, high-affect."

[cast.caller_default]
voice_model = "am_voice2"
tone_prompt = "Random listener calling in. Off-mic acoustics, slight phone-line crunch."
eq_profile  = "telephone"            # opt into the existing EQ profile
```

The host is just `cast.host` under the hood; the existing `[host]` block keeps working for compatibility.

### 2. Content modules — sources of script material

A content module is whatever raw material a script-writer would need: an RSS feed, a weather snapshot, a prompt template, a list of fictional product names to sample. Each module has a type and the fields its type needs.

```toml
[content.npr_top]
type      = "rss"
url       = "https://feeds.npr.org/1001/rss.xml"
max_items = 5

[content.local_weather]
type = "weather"                     # pulls from settings.feeds

[content.local_traffic]
type = "traffic"

[content.classic_ads]
type   = "prompt_pool"
prompt = """
Write a 30-second parody radio ad spot for a fictional product from the late 1960s.
Be over-the-top, era-appropriate, slightly absurd. End with a fake phone number or
tagline. Don't repeat products from these recent ads: {recent_ads}.
"""

[content.caller_personas]
type   = "prompt_pool"
prompt = """
Invent a brief on-air call from a listener. Give them a distinct quirk
(occupation, age, accent, oddity). Keep their utterance under 30 words and
slightly weird.
"""
```

New types are easy to add — each one is a small Rust struct + a `fn resolve(ctx) -> ContentMaterial`.

### 3. Skill bindings — when to fire, what to pull, who delivers

A skill binding wires content → cast → schedule. The actual scripting logic (turn the content into a spoken segment) is shared.

```toml
[skill.top_of_hour]
content     = "npr_top"
delivered_by = "news_anchor"         # default would be "host"
time_score  = { "0-4" = 100 }        # minute-of-hour
max_words   = 100

[skill.weather]
content     = "local_weather"
delivered_by = "host"
time_score  = { "24-27" = 50, "54-58" = 50, "*" = 10 }
max_words   = 40

[skill.fake_ad]
content     = "classic_ads"
delivered_by = "commercial_actor"
time_score  = { "*" = 10 }
max_words   = 120

[skill.caller]
content     = "caller_personas"
delivered_by = "caller_default"
time_score  = { "*" = 5 }            # rare filler
max_words   = 80

[skill.track_intro]                  # unchanged — track-bound, host-voiced
delivered_by = "host"
max_words   = 40
```

`time_score` becomes a small DSL of minute ranges → priority — replaces the hardcoded `time_score(minute)` functions in each skill module. `*` is the baseline.

## Module-level prompt composition

A rendered segment's system prompt becomes:

```
You are {cast.name}, on {host.callsign}.
{cast.tone_prompt}

You are delivering: {content.kind} ({content.description}).
{content.prompt OR content-type-specific prefix}
```

So `news_anchor` doesn't get the host's `tone_prompt`. The commercial actor doesn't either. The host's prompt is only used when `delivered_by = "host"`.

## How this plays with what exists

| Existing piece | Survives | Changes |
|---|---|---|
| `[host]` block | yes | aliased to `cast.host`; field for field |
| `[host.skills]` toggles | yes | drives whether `skill.<name>` is built |
| `[host.skill_config.X]` overrides | yes | merged onto `[skill.X]` for backcompat |
| `KokoroTts` subprocess | yes | now invoked per-cast-member, not per-station |
| `LlmRouter` | yes | cast member picks `llm_backend`; default still `ollama` |
| `time_score(minute)` per skill | replaced | data, not code: `time_score = {...}` in TOML |
| `eq_profile` per station | extended | per-cast-member, overrides station default |

## What we'd need to build

- `Cast` + `CastMember` types, persona TOML loader extended.
- `Content` enum with `Rss`, `Weather`, `Traffic`, `PromptPool` variants. Existing `feeds/` modules satisfy three of them already.
- One generic `Skill` impl driven by `[skill.*]` config (replaces 8 hardcoded skill structs). The old per-file skills become 80% smaller.
- Per-cast-member TTS — render through whichever Kokoro voice is configured, so one segment can be voiced by Walter and the next by Donna.
- `time_score` parser (minute-range → priority).

## Open questions

1. **Where do cast members live?** Same persona TOML (per-station), or shared `cast/*.toml` that stations can import? Shared makes it easy to reuse "Walter the news guy" across multiple stations; per-station keeps things simple.
2. **Caller voices: one fixed cast member, or sample per call?** Right now `caller_default` is fixed; could pool 3-4 caller voices and pick randomly to vary the feel.
3. **Ad spot pools — generated each time or rendered once and re-played?** Pre-rendering classic ads (run once, play 50× over a week) saves LLM/TTS cost dramatically and matches how real radio does it.
4. **Imager/jingle support.** Same shape: a `content.type = "audio_file"` module that plays a fixed file. Cheap to bolt on later.

## Backwards compatibility

Existing personas (Donna, Mitch) keep working untouched. The loader:
1. Reads `[host]` and treats it as `cast.host`.
2. For each enabled `[host.skills]` toggle, materialises a default `skill.<name>` binding pointing at `cast.host` with the hardcoded current prompt as the implicit content module.
3. Any new `[cast]` / `[content]` / `[skill]` blocks shadow the defaults.

No persona has to be rewritten unless they want the new features.
