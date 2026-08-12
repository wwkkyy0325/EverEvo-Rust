# syntax=docker/dockerfile:1
# ────────────────────────────────────────────────────────────────────────────
# everevo-gaia — GAIA benchmark runtime image.
#
# Each GAIA task runs in a FRESH container from this image:
#   - Linux everevo-server binary (built by scripts/build_linux_binary.sh)
#   - chromium (browser_bridge tool), python3 + openpyxl (xlsx attachments)
#   - Empty /data with a bootstrap marker → server skips asset download,
#     RAG auto-disables (no embedding models mounted) → zero domain-knowledge
#     leakage, matching "anti-cheating" benchmark requirements.
#   - config.toml pins deepseek-v4-flash (all providers) + fully_auto sandbox.
#
# Build:   docker build --build-arg DEEPSEEK_API_KEY=sk-... -t everevo-gaia scripts/gaia-docker/
# Run:     docker run --rm -d -p 127.0.0.1:13456:13456 everevo-gaia
# ────────────────────────────────────────────────────────────────────────────

FROM debian:bookworm-slim

# DeepSeek API key is injected at build time via --build-arg so the plaintext
# secret never lands in the Dockerfile / git.
ARG DEEPSEEK_API_KEY

# ── System deps: browser bridge + attachment parsing + CA certs ───────────
RUN apt-get update && apt-get install -y --no-install-recommends \
        chromium \
        ca-certificates \
        python3 \
        python3-openpyxl \
        poppler-utils \
        unzip \
        curl \
        fonts-liberation \
    && rm -rf /var/lib/apt/lists/*

# Chromium wrapper: inside the container we run as root, where Chromium's
# sandbox is unusable — `browser_bridge` launches chromium via find_browser()
# (which::which) WITHOUT --no-sandbox, so it would crash on spawn. Provide a
# wrapper earlier in PATH that injects --no-sandbox + --disable-dev-shm-usage
# (the latter avoids /dev/shm exhaustion with the default 64MB tmpfs).
RUN printf '#!/bin/sh\nexec /usr/bin/chromium --no-sandbox --disable-dev-shm-usage "$@"\n' \
        > /usr/local/bin/chromium \
    && chmod +x /usr/local/bin/chromium

# ── EverEvo server binary (built separately, see build_linux_binary.sh) ──
COPY everevo-server /usr/local/bin/everevo-server
RUN chmod +x /usr/local/bin/everevo-server

# ── Bootstrap marker: skip onnx-runtime/python/bge download at first boot ──
#    cmd_serve() checks data/.everevo_init → "Already provisioned" branch.
RUN mkdir -p /data/memory /data/config /data/sandbox \
    && touch /data/.everevo_init

# ── LLM + sandbox config (all providers = deepseek-v4-flash) ──────────────
#    NOTE: embedding_model intentionally omitted → ModelRegistry finds no
#    models → RAG disabled at startup ("RAG disabled, starting without models").
#    default_permission_level is NOT read from config.toml — it is set via
#    EVEREVO_PERMISSION_LEVEL env (see AppConfig::load in everevo-core).
#    api_key is $DEEPSEEK_API_KEY (build arg, expanded by the unquoted heredoc).
#    Fail the build early if the arg is missing — a config with an empty key
#    would otherwise build fine and 401 on every LLM call at benchmark time.
RUN test -n "$DEEPSEEK_API_KEY" \
    || { echo "ERROR: DEEPSEEK_API_KEY build arg is required (docker build --build-arg DEEPSEEK_API_KEY=sk-...)" >&2; exit 1; }
RUN cat > /data/config.toml <<EOF
[[llm]]
id = "d08d601a-1576-44fd-8631-863b269f9e46"
api_format = "anthropic"
api_key = "$DEEPSEEK_API_KEY"
base_url = "https://api.deepseek.com/anthropic"
model = "deepseek-v4-flash"

[[llm]]
id = "798f814c-fd4e-403e-b9fb-433c644ea4f2"
api_format = "anthropic"
api_key = "$DEEPSEEK_API_KEY"
base_url = "https://api.deepseek.com/anthropic"
model = "deepseek-v4-flash"

[[llm]]
id = "3217a6d4-753a-45f7-93ca-19deb457314d"
api_format = "anthropic"
api_key = "$DEEPSEEK_API_KEY"
base_url = "https://api.deepseek.com/anthropic"
model = "deepseek-v4-flash"

[routing]
mainModelId = "798f814c-fd4e-403e-b9fb-433c644ea4f2"
mainEffort = "high"

[[routing.tiers]]
modelId = "798f814c-fd4e-403e-b9fb-433c644ea4f2"
effort = "high"
EOF

# ── Runtime ───────────────────────────────────────────────────────────────
# full_auto → shell tool runs without human confirmation (unattended container)
ENV EVEREVO_DATA_DIR=/data \
    EVEREVO_PERMISSION_LEVEL=fully_auto
EXPOSE 13456
CMD ["everevo-server", "serve", "--host", "0.0.0.0", "--port", "13456"]
