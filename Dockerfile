# syntax=docker/dockerfile:1.7

# ───────────────────────────── builder ─────────────────────────────
# BuildKit cache mounts persist the cargo registry + target dir across
# rebuilds, so iterative builds only recompile what changed. Falls back
# to a normal (uncached) build on legacy Docker clients.
#
# Pin to the same minor that CI's `dtolnay/rust-toolchain@stable` is
# currently shipping — keeps the builder in lockstep with the
# `cargo test`/`cargo clippy` job so transitive-dep MSRV bumps surface
# in both places at once. Floor today is 1.86 (icu_* family), but pinning
# higher avoids re-bumping every time a dep slides forward.
FROM rust:1.95-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release --locked --bin airtime \
 && cp target/release/airtime /usr/local/bin/airtime \
 && strip /usr/local/bin/airtime

# ──────────────────────────── runtime ─────────────────────────────
# Includes:
#   - ffmpeg      → loudnorm, EQ filter chains, Ogg/FLAC re-mux for Icecast push
#   - python3     → host for the Kokoro shim that matches our subprocess interface
#   - kokoro-onnx → local TTS (CPU inference via onnxruntime)
#
# The image stays lossless end-to-end: ffmpeg is invoked with `-c:a copy`
# whenever it touches a FLAC, and the source client only emits Ogg/FLAC.
FROM debian:bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive \
    RUST_LOG=info \
    AIRTIME_SETTINGS=/app/settings.toml \
    AIRTIME_PERSONAS=/app/personas \
    KOKORO_BIN=/usr/local/bin/kokoro

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        ffmpeg \
        ca-certificates \
        python3 \
        python3-pip \
        libsndfile1 \
        procps \
        tini \
        wget \
 && rm -rf /var/lib/apt/lists/*

# kokoro-onnx pulls in onnxruntime + numpy + soundfile; ~500MB but
# self-contained. `--break-system-packages` is required on Debian
# Bookworm (PEP 668), which is fine inside a container.
RUN pip3 install --no-cache-dir --break-system-packages \
        "kokoro-onnx>=0.5.0" \
        "soundfile>=0.12" \
        "numpy>2.0.2"

WORKDIR /app

# Kokoro shim — matches the subprocess CLI airtime expects.
COPY docker/kokoro /usr/local/bin/kokoro
RUN chmod +x /usr/local/bin/kokoro

# Default config & personas; either can be overridden by bind-mounting.
COPY docker/settings.toml /app/settings.toml
COPY personas /app/personas

COPY --from=builder /usr/local/bin/airtime /usr/local/bin/airtime

# Scratch dir for audio, plus mount-points for music + Kokoro models.
RUN mkdir -p /tmp/airtime /music /app/models

VOLUME ["/music", "/app/models", "/tmp/airtime"]

# A simple liveness probe: airtime exits non-zero on bad settings, so if
# the process is up, settings parsed OK. Listeners reach the audio via
# Icecast, not the airtime process, so there's no HTTP port to probe here.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD pgrep -x airtime >/dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/airtime"]
