#!/bin/bash
# Full-53 GAIA L1 launch — benchmark mode, meta-agent OFF.
# Env notes:
#   - EVEREVO_BENCHMARK=1 gates budget/convergence/fully_auto/venue behavior.
#   - EVEREVO_META_AGENT=0 disables the meta-agent self-diagnosis loop (switch
#     API: routing `metaAgentEnabled`, or `EVEREVO_META_AGENT`).
#   - Fresh HF_HOME/HF_DATASETS_CACHE: quarantines the previous GAIA caches so
#     the model never sees a prior run's downloads (anti-contamination).
#   - HF_TOKEN is scrubbed from the spawned server's env by gaia_bench.py
#     start_server(), so the sandboxed agent never has dataset credentials.
#   - HF_TOKEN must come from the CALLER's environment — never hardcode a
#     token in this file (it is committed to git). If unset, the script
#     exits immediately below with a clear message.
set -e
: "${HF_TOKEN:?HF_TOKEN 未设置 — 运行前请先 export HF_TOKEN=...(勿提交明文到 git)}"
export HF_TOKEN
export HTTP_PROXY="http://127.0.0.1:7890"
export HTTPS_PROXY="http://127.0.0.1:7890"
export EVEREVO_BENCHMARK=1
export EVEREVO_META_AGENT=0
export HF_HOME="C:/Users/lcx/gaia-run5-hf"
export HF_DATASETS_CACHE="C:/Users/lcx/gaia-run5-hf/datasets"
taskkill //F //IM everevo-server.exe 2>/dev/null || true
sleep 3
data/bench/venv/Scripts/python.exe f:/workspace-new/wwkkyy0325/EverEvo-Rust/scripts/gaia_bench.py --level level1
