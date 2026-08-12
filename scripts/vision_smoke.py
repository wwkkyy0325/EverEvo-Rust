#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""Offline smoke test for the local vision provider (qwen3-vl-2b / llama.cpp).

Probes http://127.0.0.1:8080/v1/chat/completions; if reachable, sends a general
description prompt for two real GAIA images (q17 chess board, q22 fractions) and
prints the model's response. Exits 2 with a hint if the server isn't running.

Run with the sandbox venv python (NOT the WindowsApps stub):
    data\\bench\\venv\\Scripts\\python.exe scripts\\vision_smoke.py
"""
import base64
import json
import os
import sys
import urllib.request

ENDPOINT = "http://127.0.0.1:8080/v1/chat/completions"
MODEL = "qwen3-vl-2b-instruct"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ATTACH = os.path.join(ROOT, "data", "bench", "tooltest", "attachments")

SAMPLES = [
    ("q17 chess board", os.path.join(ATTACH, "cca530fc-4052-43b2-b130-b30968d8aa44.png"),
     "This is a chess position. Report the board as FEN and any obvious best move."),
    ("q22 fractions", os.path.join(ATTACH, "9318445f-fe6a-4e1b-acbf-c68228c9906a.png"),
     "Transcribe the worksheet exactly: all fraction numbers and operators visible."),
]


def probe() -> bool:
    """Return True if the vision server answers /v1/chat/completions."""
    payload = json.dumps({
        "model": MODEL,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 8,
    }).encode("utf-8")
    req = urllib.request.Request(
        ENDPOINT, data=payload, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            body = json.loads(resp.read().decode("utf-8"))
            return bool(body.get("choices"))
    except Exception:
        return False


def describe_image(path: str, question: str) -> str:
    with open(path, "rb") as f:
        b64 = base64.b64encode(f.read()).decode("ascii")
    payload = json.dumps({
        "model": MODEL,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image_url", "image_url": {"url": f"data:image/png;base64,{b64}"}},
                {"type": "text", "text": question},
            ],
        }],
        "max_tokens": 1024,
    }).encode("utf-8")
    req = urllib.request.Request(
        ENDPOINT, data=payload, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as resp:
        body = json.loads(resp.read().decode("utf-8"))
    return body["choices"][0]["message"]["content"]


def main() -> int:
    if not os.path.isdir(ATTACH):
        print(f"[skip] attachment dir missing: {ATTACH}")
        return 0
    if not probe():
        print("Vision server not reachable at", ENDPOINT)
        print("Start it first — see docs/ops/serve_vision_qwen.md")
        print("(llama-server -m <llm.gguf> --mmproj <mmproj.gguf> -c 32768 -ngl N "
              "--port 8080 --host 127.0.0.1)")
        return 2

    for name, path, q in SAMPLES:
        if not os.path.isfile(path):
            print(f"[skip] {name}: missing {path}")
            continue
        print(f"=== {name} ===")
        try:
            print(describe_image(path, q))
        except Exception as e:  # noqa: BLE001 — smoke test reports and continues
            print(f"[error] {name}: {e}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
