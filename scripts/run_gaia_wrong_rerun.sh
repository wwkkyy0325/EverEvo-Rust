#!/bin/bash
# Re-run ONLY the wrong questions from the completed 165q baseline, against the
# freshly-built binary WITH the verifier + meta-orchestrator fixes.
#
# Isolation: identical env to the base run (same HF_HOME cache so the retry
# sees the SAME data availability — a clean A/B of the fix, not a re-download).
# Enables the LLM-free meta-orchestrator (EVEREVO_META_ORCHESTRATOR=1) and the
# benchmark mode; 6 workers per user request (faster wall-clock; user approved).
set -e
: "${HF_TOKEN:?HF_TOKEN 未设置 — 运行前请先 export HF_TOKEN=...(勿提交明文到 git)}"
export HF_TOKEN
export HTTP_PROXY="http://127.0.0.1:7890"
export HTTPS_PROXY="http://127.0.0.1:7890"
export EVEREVO_BENCHMARK=1
export EVEREVO_META_AGENT=0
export EVEREVO_META_ORCHESTRATOR=1
export HF_HOME="${HF_HOME:-$HOME/gaia-run5-hf}"
export HF_DATASETS_CACHE="${HF_DATASETS_CACHE:-$HOME/gaia-run5-hf/datasets}"

IDS="${1:?usage: run_gaia_wrong_rerun.sh '<task_id,list>'}"
TS=$(date +%Y%m%d_%H%M%S)
LOG="data/bench/gaia-results/gaia_wrong_rerun_${TS}.log"

taskkill //F //IM everevo-server.exe 2>/dev/null || true
sleep 3

# Local vision provider (Qwen3.5-4B int8, llama.cpp:8766) must be running — the
# describe_image tool routes vision through it; without it vision questions
# silently time out. LAN IP only (loopback does NOT reach it); the IP is DHCP-
# volatile and lives in the git-ignored data/config.toml ([[llm]] id=local-qwen35-2b),
# NOT hardcoded here — read it so no private IP is ever committed.
VISION_BASE=$(data/bench/venv/Scripts/python.exe -c "
import tomllib
t = tomllib.loads(open('data/config.toml', encoding='utf-8').read())
for l in t.get('llm', []):
    if l.get('id') == 'local-qwen35-2b':
        print(l.get('base_url', '').rstrip('/'))
" 2>/dev/null)
VISION_URL="${VISION_URL:-${VISION_BASE:+$VISION_BASE/models}}"
curl -s -m 5 "$VISION_URL" >/dev/null 2>&1 && echo "Vision server UP (${VISION_URL})" | tee -a "$LOG" \
  || { echo "FATAL: vision server not reachable at $VISION_URL (set VISION_URL or configure data/config.toml [[llm]] id=local-qwen35-2b base_url)" | tee -a "$LOG"; exit 1; }

# Dual-model GAIA config: the commander (mainModelId) becomes v4-pro for the
# run; soldiers (subagentModelId) stay flash, officers (verifierModelId) stay
# pro. Backup data/config.toml, write the GAIA variant, restore on exit.
CFG=data/config.toml
CFG_BAK="data/config.toml.bak.${TS}"
cp "$CFG" "$CFG_BAK"
restore_cfg() { cp "$CFG_BAK" "$CFG"; rm -f "$CFG_BAK"; }
trap restore_cfg EXIT
data/bench/venv/Scripts/python.exe -c "
import tomllib, pathlib
p = pathlib.Path('$CFG')
t = tomllib.loads(p.read_text(encoding='utf-8'))
t['routing']['mainModelId'] = 'deepseek-v4-pro'
# tomllib can't write TOML; emit a minimal patch via string replace instead.
txt = p.read_text(encoding='utf-8')
import re
txt = re.sub(r'^(mainModelId\s*=\s*)\"[^\"]*\"', r'\1\"deepseek-v4-pro\"', txt, flags=re.M)
p.write_text(txt, encoding='utf-8')
print('GAIA config: mainModelId -> deepseek-v4-pro (dual-model)')
" || restore_cfg

# NOTE (2026-08-14): self-consistency majority voting is a PURE NEGATIVE for
# GAIA — the 37-q run gave vote=4/37 (10.8%) vs single-run 8/37 (21.6%),
# because GAIA errors are SYSTEMATIC extraction bias (the majority is wrong),
# not random noise. Keep attempts=1 (single run) for GAIA; the `--attempts N`
# flag remains available for Pass@N diagnostics only.
echo "=== Wrong-question rerun ${TS}: workers=6, attempts=1, ids=$(echo "$IDS" | tr ',' '\n' | wc -l), main=pro ===" | tee "$LOG"
data/bench/venv/Scripts/python.exe scripts/gaia_bench.py --level all --ids "$IDS" \
    --workers 6 --attempts 1 --question-timeout 1800 2>&1 | tee -a "$LOG"
echo "RERUN_DONE_LOG=$LOG"
