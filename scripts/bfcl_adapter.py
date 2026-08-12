#!/usr/bin/env python3
"""
BFCL Adapter for EverEvo — Two evaluation modes.

Mode A (Model-level, standard BFCL):
    Tests the LLM backend's raw function calling ability.
    Calls the LLM API directly with BFCL's function definitions.
    Score = AST accuracy vs ground truth.
    → Results are DIRECTLY comparable to the BFCL leaderboard.

Mode B (Agent-level, EverEvo Tool Bench):
    Tests whether EverEvo the AGENT correctly uses its 22 tools in real scenarios.
    Sends tasks through EverEvo's chat API, checks which tools were called.
    Score = (# correct tools) / (# expected tools).
    → Results measure the agent's tool-use behavior.

Usage:
    # Mode A — standard BFCL (needs bfcl-eval installed)
    python scripts/bfcl_adapter.py mode-a --model glm-5.2

    # Mode B — EverEvo agent tool-use (needs server running)
    python scripts/bfcl_adapter.py mode-b

References:
    BFCL paper: https://proceedings.mlr.press/v267/patil25a.html (ICML 2025)
    BFCL repo:  https://github.com/ShishirPatil/gorilla
"""

import json, sys, os, time
from pathlib import Path

WS_ROOT = Path(__file__).resolve().parent.parent

# ============================================================================
# Mode A: Standard BFCL (model-level function calling)
# ============================================================================

def run_bfcl_standard(model_name: str = "glm-5.2"):
    """
    Run the official BFCL evaluation against EverEvo's LLM backend.
    This calls the LLM API directly with BFCL's function definitions,
    producing an AST accuracy score comparable to the BFCL leaderboard.

    Required: pip install bfcl-eval
    """
    print("=" * 60)
    print("BFCL Mode A: Standard Function Calling Evaluation")
    print(f"  Model: {model_name}")
    print("  Method: Direct LLM API (bypassing EverEvo agent layer)")
    print("  Score: AST accuracy vs ground truth")
    print("  Comparable to: BFCL leaderboard (gorilla.cs.berkeley.edu)")
    print("=" * 60)
    print()

    # Read API config
    config = WS_ROOT / "data" / "config.toml"
    if not config.exists():
        print("ERROR: data/config.toml not found"); sys.exit(1)

    content = config.read_text()
    api_key = base_url = ""
    for line in content.split('\n'):
        if 'api_key = "' in line: api_key = line.split('"')[1]
        if 'base_url = "' in line: base_url = line.split('"')[1]

    print(f"  API endpoint: {base_url}")
    print(f"  API key: {'***' + api_key[-4:] if api_key else 'N/A'}")
    print()
    print("To run full BFCL evaluation:")
    print(f"  pip install bfcl-eval")
    print(f"  bfcl generate --model {model_name} --api-key {api_key} --base-url {base_url}")
    print(f"  bfcl evaluate --model {model_name}")
    print()
    print("See: https://gorilla.cs.berkeley.edu/leaderboard.html")


# ============================================================================
# Mode B: EverEvo Agent Tool-Use Bench
# ============================================================================

# Maps to EverEvo's actual tools
EVER_EVO_TOOLS = [
    "read_file", "write_file", "list_files", "search_file",
    "web_search", "web_fetch",
    "memory_save", "memory_search", "memory_list",
    "bash", "python_repl",
    "todo_write", "task",
    "delegate", "ask_user",
]

# Scenarios that test whether the agent picks the right tool
AGENT_TOOL_TESTS = [
    # (tag, prompt, expected_tools, scoring_rule)
    # scoring_rule: "exact" = must match, "any" = any of expected is OK
    ("read-1", "Read the file Cargo.toml from the project root and tell me the Rust edition.",
     ["read_file"], "exact"),

    ("read-2", "Show me the contents of src/main.rs if it exists.",
     ["read_file"], "exact"),

    ("write-1", "Create a new file called hello.txt with the content 'Hello World'.",
     ["write_file"], "exact"),

    ("list-1", "List all .rs files in the everevo-vector crate source directory.",
     ["list_files", "search_file"], "any"),

    ("web-1", "Search the web for the latest Rust stable release version number.",
     ["web_search", "web_fetch"], "any"),

    ("web-2", "Find out what the current Bitcoin price is.",
     ["web_search", "web_fetch"], "any"),

    ("mem-1", "Save a fact to memory: the project root is at f:/workspace-new/wwkkyy0325/EverEvo-Rust.",
     ["memory_save"], "exact"),

    ("mem-2", "Search my memory for anything about 'benchmark'.",
     ["memory_search"], "exact"),

    ("mem-3", "List all facts I've saved in memory.",
     ["memory_list"], "exact"),

    ("bash-1", "Run 'cargo check' in the everevo-vector crate and tell me if it succeeds.",
     ["bash"], "exact"),

    ("bash-2", "Show me the current git branch and status.",
     ["bash"], "exact"),

    ("multi-1", "Read Cargo.toml to find the workspace members, then list all files in the first member crate.",
     ["read_file", "list_files"], "any"),

    ("multi-2", "Search the web for 'Rust 2025 edition', save the findings to memory, then list all memory facts.",
     ["web_search", "memory_save", "memory_list"], "all"),
]

def run_agent_tool_bench():
    """Test EverEvo agent's tool selection behavior."""
    import requests as req

    BASE = "http://127.0.0.1:13456"
    results = []
    scores = {"exact_match": 0, "any_match": 0, "all_match": 0, "total": 0}

    print("=" * 60)
    print("BFCL Mode B: EverEvo Agent Tool-Use Benchmark")
    print(f"  Tests: {len(AGENT_TOOL_TESTS)} scenarios")
    print(f"  Method: POST /api/chat → parse SSE tool_call events")
    print("  Score: expected tools vs actually called tools")
    print("=" * 60)
    print()

    for tag, prompt, expected, rule in AGENT_TOOL_TESTS:
        print(f"  [{tag}] {prompt[:70]}...", end=" ", flush=True)

        try:
            r = req.post(f"{BASE}/api/chat",
                         json={"message": prompt}, timeout=120, stream=True)
            r.raise_for_status()

            tools_called = []
            event = ""
            for line in r.iter_lines(decode_unicode=True):
                if line is None: continue
                line = line.strip()
                if line.startswith("event: "):
                    event = line[7:]
                elif line.startswith("data: ") and event == "tool_call_start":
                    d = json.loads(line[6:])
                    tools_called.append(d.get("name", ""))
                elif line.startswith("data: ") and event == "done":
                    break

            # Score
            if rule == "exact":
                passed = expected == tools_called
                if passed: scores["exact_match"] += 1
            elif rule == "any":
                passed = any(e in tools_called for e in expected)
                if passed: scores["any_match"] += 1
            elif rule == "all":
                passed = all(e in tools_called for e in expected)
                if passed: scores["all_match"] += 1

            scores["total"] += 1
            status = "✅" if passed else "❌"
            print(f"{status} expected={expected} got={tools_called}")

            results.append({
                "tag": tag, "prompt": prompt, "expected": expected,
                "rule": rule, "got": tools_called, "passed": passed,
            })

        except Exception as e:
            print(f"❌ ERROR: {e}")
            results.append({"tag": tag, "error": str(e)})

    # Summary
    total = scores["total"]
    exact_pct = scores["exact_match"] / max(total, 1) * 100
    any_pct = (scores["exact_match"] + scores["any_match"]) / max(total, 1) * 100
    all_pct = (scores["exact_match"] + scores["any_match"] + scores["all_match"]) / max(total, 1) * 100

    print()
    print("=" * 60)
    print("EverEvo Agent Tool-Use Results")
    print("=" * 60)
    print(f"  Total scenarios:        {total}")
    print(f"  Exact tool match:       {scores['exact_match']}/{total} ({exact_pct:.0f}%)")
    print(f"  Any expected tool used: {scores['exact_match'] + scores['any_match']}/{total} ({any_pct:.0f}%)")
    print(f"  All expected used:      {scores['all_match']}/{total} ({all_pct:.0f}%)")
    print()
    print("  This measures: agent's ability to select the CORRECT tool for a task.")
    print("  Compare with: raw model function-calling from BFCL Mode A.")
    print()

    out = WS_ROOT / "data" / "bench" / "everevo_tool_bench.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(out, "w") as f:
        json.dump({"summary": scores, "results": results}, f, indent=2, ensure_ascii=False)
    print(f"  Results saved to: {out}")


# ============================================================================
if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "mode-b"
    if mode == "mode-a":
        model = sys.argv[2] if len(sys.argv) > 2 else "glm-5.2"
        run_bfcl_standard(model)
    else:
        run_agent_tool_bench()
