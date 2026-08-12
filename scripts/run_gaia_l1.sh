#!/bin/bash
set -e
: "${HF_TOKEN:?HF_TOKEN 未设置 — 运行前请先 export HF_TOKEN=...(勿提交明文到 git)}"
export HF_TOKEN
export HTTP_PROXY="http://127.0.0.1:7890"
export HTTPS_PROXY="http://127.0.0.1:7890"
taskkill //F //IM everevo-server.exe 2>/dev/null
sleep 3
C:/Users/lcx/.local/share/TeleAgent/runtimes/python/python.exe f:/workspace-new/wwkkyy0325/EverEvo-Rust/scripts/gaia_bench.py --level level1
