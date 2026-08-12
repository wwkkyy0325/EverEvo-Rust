#!/usr/bin/env python3
"""
Verify EverEvo token estimation accuracy by comparing against real LLM API responses.

Usage: python scripts/verify_tokens.py
"""
import json, sys, os
from pathlib import Path
import urllib.request

WS_ROOT = Path(__file__).resolve().parent.parent

def load_config():
    """Read API config from data/config.toml (first [[llm]] block)."""
    config_path = WS_ROOT / "data" / "config.toml"
    if not config_path.exists():
        print("ERROR: data/config.toml not found")
        sys.exit(1)

    content = config_path.read_text()
    # Parse [[llm]] TOML manually (avoid toml dependency)
    model = api_key = base_url = None
    for line in content.split('\n'):
        line = line.strip()
        if 'model = "' in line or "model = '" in line:
            model = line.split('"')[1] if '"' in line else line.split("'")[1]
        if 'api_key = "' in line or "api_key = '" in line:
            api_key = line.split('"')[1] if '"' in line else line.split("'")[1]
        if 'base_url = "' in line or "base_url = '" in line:
            base_url = line.split('"')[1] if '"' in line else line.split("'")[1]
        if model and api_key and base_url:
            break

    if not all([model, api_key, base_url]):
        print(f"ERROR: could not parse config. Found: model={model}, key={'***' if api_key else 'None'}, url={base_url}")
        sys.exit(1)
    return model, api_key, base_url

def estimate_tokens(text: str) -> int:
    """Same logic as agent_bench.py: ~3 chars per token."""
    return max(1, len(text) // 3)

def call_llm_real(model: str, api_key: str, base_url: str, prompt: str):
    """Call LLM API directly and get REAL token counts."""
    endpoint = f"{base_url.rstrip('/')}/messages"
    body = {
        "model": model, "max_tokens": 256, "temperature": 0.0,
        "system": "You are a helpful assistant.",
        "messages": [{"role": "user", "content": prompt}],
    }
    req = urllib.request.Request(
        endpoint,
        data=json.dumps(body).encode(),
        headers={
            "x-api-key": api_key,
            "anthropic-version": "2023-06-01",
            "Content-Type": "application/json",
        },
    )
    resp = urllib.request.urlopen(req, timeout=30)
    data = json.loads(resp.read())

    real_input = data.get("usage", {}).get("input_tokens", "N/A")
    real_output = data.get("usage", {}).get("output_tokens", "N/A")
    answer = data.get("content", [{}])[0].get("text", "") if "content" in data else str(data)[:200]
    return real_input, real_output, answer

# ── Test ────────────────────────────────────────────────────────────────────
model, api_key, base_url = load_config()
print(f"Model: {model}")
print(f"Endpoint: {base_url}")
print()

test_prompts = [
    ("short", "What is 2+2?"),
    ("medium", "Explain the concept of ownership in Rust and how it prevents memory safety issues. Be concise."),
    ("long", "Write a Python function that takes a list of integers and returns a new list with all duplicates removed, "
             "sorted in descending order. Include a docstring, type hints, and an example usage. "
             "The function should be efficient for large inputs and handle edge cases like empty lists."),
]

print(f"{'Type':<8} {'Est. Input':>10} {'Real Input':>10} {'Error %':>8} | {'Est. Output':>10} {'Real Output':>10} {'Error %':>8}")
print("-" * 80)

total_est_in, total_real_in = 0, 0
total_est_out, total_real_out = 0, 0

for tag, prompt in test_prompts:
    est_in = estimate_tokens(prompt) + 2000  # +2000 for context pipeline overhead
    real_in, real_out, answer = call_llm_real(model, api_key, base_url, prompt)
    est_out = estimate_tokens(answer)

    in_err = abs(est_in - int(real_in)) / max(int(real_in), 1) * 100
    out_err = abs(est_out - int(real_out)) / max(int(real_out), 1) * 100

    print(f"{tag:<8} {est_in:>10,} {str(real_in):>10} {in_err:>7.1f}% | {est_out:>10,} {str(real_out):>10} {out_err:>7.1f}%")

    total_est_in += est_in
    total_real_in += int(real_in)
    total_est_out += est_out
    total_real_out += int(real_out)

print("-" * 80)
in_err_total = abs(total_est_in - total_real_in) / max(total_real_in, 1) * 100
out_err_total = abs(total_est_out - total_real_out) / max(total_real_out, 1) * 100
print(f"{'TOTAL':<8} {total_est_in:>10,} {total_real_in:>10,} {in_err_total:>7.1f}% | {total_est_out:>10,} {total_real_out:>10,} {out_err_total:>7.1f}%")
print()
if in_err_total < 20 and out_err_total < 30:
    print("✅ Token estimation is reasonably accurate (within 20-30% of real API counts)")
    print("   The benchmark token metrics can be trusted for comparative analysis.")
else:
    print(f"⚠️  Token estimation error is high ({in_err_total:.0f}%/{out_err_total:.0f}%)")
    print("   Consider adjusting the estimate_tokens() divisor or measuring real tokens.")
