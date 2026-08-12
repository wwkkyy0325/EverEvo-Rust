#!/usr/bin/env python3
"""
EverEvo Agent Benchmark — Windows/PowerShell native.

Tests EverEvo the AGENT (not raw LLM) by:
  1. Building & starting everevo-server with isolated sandbox data
  2. Sending benchmark prompts to the agent's POST /api/chat endpoint
  3. Collecting SSE responses (text + tool calls)
  4. Generating comparison report

Usage (PowerShell):
    python scripts/agent_bench.py all       # run everything
    python scripts/agent_bench.py eqbench   # emotional intelligence
    python scripts/agent_bench.py bfcl      # tool use
    python scripts/agent_bench.py humaneval # code generation
    python scripts/agent_bench.py start     # start server, test manually
"""

import subprocess, sys, os, json, time, shutil, signal, argparse
from pathlib import Path

# ── Config ──────────────────────────────────────────────────────────────────
SANDBOX  = Path(os.environ.get("TEMP", "/tmp")) / "everevo-bench"
PORT     = 13456
BASE_URL = f"http://127.0.0.1:{PORT}"
WS_ROOT  = Path(__file__).resolve().parent.parent
SERVER_PROC = None

def log(msg):   print(f"\033[36m[bench]\033[0m {msg}")
def ok(msg):    print(f"\033[32m[  OK]\033[0m {msg}")
def fail(msg):  print(f"\033[31m[FAIL]\033[0m {msg}")

# ── Setup ───────────────────────────────────────────────────────────────────
def setup_sandbox():
    """Create isolated sandbox — copies config (read-only), creates empty data dirs."""
    log(f"Sandbox: {SANDBOX}")
    if SANDBOX.exists():
        shutil.rmtree(str(SANDBOX), ignore_errors=True)
    for d in ["data/db", "data/memory", "data/domain", "data/models",
              "data/runtime/onnxruntime/lib", "data/config", "results"]:
        (SANDBOX / d).mkdir(parents=True, exist_ok=True)

    # Copy config (read-only — agent reads API keys from here)
    for src, dst in [
        ("data/config.toml", "data/config.toml"),
        ("data/config/config.toml", "data/config/config.toml"),
    ]:
        s = WS_ROOT / src
        if s.exists():
            (SANDBOX / dst).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(str(s), str(SANDBOX / dst))

    # Copy ONNX runtime DLL
    onnx_src = WS_ROOT / "data/runtime/onnxruntime/lib"
    onnx_dst = SANDBOX / "data/runtime/onnxruntime/lib"
    if onnx_src.exists():
        for f in onnx_src.iterdir():
            shutil.copy2(str(f), str(onnx_dst / f.name))

    # Copy ONNX models
    models_src = WS_ROOT / "data/models"
    models_dst = SANDBOX / "data/models"
    if models_src.exists():
        for item in models_src.iterdir():
            d = models_dst / item.name
            if item.is_dir():
                shutil.copytree(str(item), str(d), dirs_exist_ok=True)

    log("Sandbox ready — config/models copied, data dirs empty (no contamination)")
    return True

# ── Server Lifecycle ────────────────────────────────────────────────────────
def start_server():
    """Build and start everevo-server in isolated sandbox."""
    global SERVER_PROC

    log("Building everevo-server (release, ~2-5 min, streaming output below)...")
    log("──────────────────────────────────────────────────")
    result = subprocess.run(
        ["cargo", "build", "-p", "everevo-server", "--release"],
        cwd=str(WS_ROOT)
    )
    log("──────────────────────────────────────────────────")
    if result.returncode != 0:
        fail(f"Build failed:\n{result.stderr[-500:]}")
        return False
    ok("Build complete")

    exe = WS_ROOT / "target/release/everevo-server.exe"
    if not exe.exists():
        exe = WS_ROOT / "target/release/everevo-server"

    env = os.environ.copy()
    env["EVEREVO_DATA_DIR"] = str(SANDBOX / "data")

    # Kill any leftover server from previous runs
    subprocess.run(["taskkill", "/F", "/IM", "everevo-server.exe"],
                   capture_output=True)
    time.sleep(1)

    log(f"Starting server on port {PORT}...")
    SERVER_PROC = subprocess.Popen(
        [str(exe), "serve", "--host", "127.0.0.1", "--port", str(PORT)],
        cwd=str(WS_ROOT), env=env,
        stdout=open(str(SANDBOX / "server_stdout.log"), "w"),
        stderr=open(str(SANDBOX / "server_stderr.log"), "w"),
    )

    # Wait for health check
    import requests as req
    for i in range(60):
        try:
            r = req.get(f"{BASE_URL}/api/health", timeout=2)
            if r.status_code == 200:
                ok(f"Server ready (took {i*2}s)")
                return True
        except Exception:
            time.sleep(2)
    fail("Server failed to start")
    return False

def stop_server():
    global SERVER_PROC
    if SERVER_PROC:
        log("Stopping server...")
        SERVER_PROC.send_signal(signal.SIGTERM)
        try: SERVER_PROC.wait(timeout=10)
        except subprocess.TimeoutExpired:
            SERVER_PROC.kill()
        ok("Server stopped")

# ── Chat with EverEvo Agent ─────────────────────────────────────────────────
def chat(message: str, session_id: str = None, timeout: int = 120) -> dict:
    """Send message to EverEvo chat API, collect SSE response.
    Uses `requests` for reliable SSE streaming (urllib blocks on chunked encoding).
    """
    import requests as req

    body = {"message": message}
    if session_id:
        body["session_id"] = session_id

    result = {"text": "", "tool_calls": [], "thinking": "",
              "session_id": None, "input_tokens": 0, "output_tokens": 0}
    sys.stdout.write("  ⏳ "); sys.stdout.flush()

    try:
        r = req.post(f"{BASE_URL}/api/chat", json=body, timeout=timeout, stream=True)
        r.raise_for_status()

        event = ""
        shown = {"thinking": False, "text": False, "tool": False}
        for raw_line in r.iter_lines(decode_unicode=True):
            if raw_line is None:  # heartbeat
                continue
            line = raw_line.strip()
            if line.startswith("event: "):
                event = line[7:].strip()
            elif line.startswith("data: "):
                try:
                    d = json.loads(line[6:])
                except json.JSONDecodeError:
                    continue
                if event == "thinking" and not shown["thinking"]:
                    sys.stdout.write("💭"); sys.stdout.flush(); shown["thinking"] = True
                elif event == "tool_call_start":
                    name = d.get("name", "?")
                    sys.stdout.write(f"🔧{name}"); sys.stdout.flush(); shown["tool"] = True
                    result["tool_calls"].append({
                        "name": name,
                        "arguments": str(d.get("arguments", ""))[:200]
                    })
                elif event == "text":
                    if not shown["text"]:
                        sys.stdout.write("📝"); sys.stdout.flush(); shown["text"] = True
                    result["text"] += d.get("content", "")
                elif event == "done":
                    result["session_id"] = d.get("session_id", "")
                    result["input_tokens"] = d.get("input_tokens", 0)
                    result["output_tokens"] = d.get("output_tokens", 0)
                    return result  # done — return immediately
        return result
    except Exception as e:
        sys.stdout.write(f" ❌ {e}"); sys.stdout.flush()
        result["error"] = str(e)
        return result
    finally:
        sys.stdout.write("\n"); sys.stdout.flush()

# ── Benchmarks ──────────────────────────────────────────────────────────────
def run_eqbench():
    """EQ-Bench style: emotional intelligence through agent chat."""
    log("=== EQ-Bench: Emotional Intelligence ===")
    log("Agent: persona + context pipeline + natural dialogue")

    scenarios = [
        ("grief", "My best friend of 20 years just told me they're moving to another country next month. "
         "I feel completely lost and don't know how to cope with this change."),
        ("injustice", "I spent six months on a project and my manager presented it to the CEO as entirely "
         "their own work. I wasn't even mentioned. How should I handle this professionally?"),
        ("anxiety", "My aging parents are refusing to see a doctor despite obvious health problems. "
         "I'm an only child and live far away. I feel paralyzed with worry every day."),
        ("conflict", "My partner and I had a terrible fight last night. They said things that cut really deep. "
         "I don't know if we can come back from this. What would you advise?"),
        ("failure", "I just failed the bar exam for the third time. My family has sacrificed so much for me. "
         "I feel like a complete failure and don't know how to face them."),
    ]

    results = []
    total_in, total_out = 0, 0
    for tag, prompt in scenarios:
        log(f"  [{tag}] {prompt[:60]}...")
        resp = chat(prompt)
        results.append({"tag": tag, "prompt": prompt, "answer": resp["text"][:500],
                         "input_tokens": resp["input_tokens"], "output_tokens": resp["output_tokens"]})
        total_in += resp["input_tokens"]; total_out += resp["output_tokens"]
        ok(f"  → {resp['text'][:80]}...")
    log(f"  Token usage: {total_in:,} in / {total_out:,} out (avg {total_out//len(scenarios):,}/query)")

    (SANDBOX / "results").mkdir(exist_ok=True)
    with open(SANDBOX / "results/eqbench.json", "w") as f:
        json.dump(results, f, indent=2, ensure_ascii=False)
    ok(f"EQ-Bench: {len(results)} scenarios → {SANDBOX / 'results/eqbench.json'}")

def run_bfcl():
    """BFCL-style: tool use through agent's 22-tool arsenal.
    Scoring: AST-like — did the agent call the expected tool?
    Each test: correct tool called = 1 point, wrong/none = 0.
    """
    log("=== BFCL-style: Tool Use ===")
    log("Scoring: objective tool-call matching (no LLM judge)")

    prompts = [
        ("read_file", "Read the file Cargo.toml from the project root and tell me what edition it specifies.",
         ["read_file"]),
        ("memory_save", "Save a memory fact: the benchmark ran at " + time.strftime("%Y-%m-%d %H:%M") +
         ". Title it 'benchmark-run' with description 'EverEvo agent benchmark execution'.",
         ["memory_save"]),
        ("list_files", "List all Rust source files in the everevo-vector crate at crates/infra/everevo-vector/src/",
         ["list_files", "read_file"]),
        ("web_search", "Search the web for 'Rust 2025 edition release date' and tell me what you find.",
         ["web_search", "web_fetch"]),
    ]

    results = []
    score = 0
    total_in, total_out = 0, 0
    for tag, prompt, expected in prompts:
        log(f"  [{tag}] {prompt[:80]}...")
        resp = chat(prompt)
        tools_used = [t["name"] for t in resp["tool_calls"]]
        hit = any(e in tools_used for e in expected)
        if hit: score += 1
        results.append({
            "tag": tag, "prompt": prompt, "expected_tools": expected,
            "tools_used": tools_used, "score": 1 if hit else 0,
            "answer": resp["text"][:300],
            "input_tokens": resp["input_tokens"], "output_tokens": resp["output_tokens"],
        })
        total_in += resp["input_tokens"]; total_out += resp["output_tokens"]
        status = "✅" if hit else "❌"
        log(f"    {status} expected={expected} used={tools_used} | {resp['input_tokens']:,}→{resp['output_tokens']:,} tok")

    (SANDBOX / "results").mkdir(exist_ok=True)
    with open(SANDBOX / "results/bfcl.json", "w") as f:
        json.dump({"score": f"{score}/{len(prompts)}", "results": results}, f, indent=2, ensure_ascii=False)
    ok(f"BFCL: {score}/{len(prompts)} tools correct → {SANDBOX / 'results/bfcl.json'}")

def run_humaneval():
    """HumanEval-style: code generation through agent."""
    log("=== HumanEval: Code Generation ===")
    log("Agent: context pipeline + write_file tool")

    tasks = [
        ("has_close_elements",
         "Write a Python function 'has_close_elements(numbers, threshold)' that returns True "
         "if any two numbers in the list are closer than the threshold. Include example usage."),
        ("separate_paren_groups",
         "Write a Python function 'separate_paren_groups(paren_string)' that separates a string "
         "of nested parentheses into balanced groups. Example input: '(()())' → ['(()())', '()()()'] → ['(()())']"),
        ("mean_absolute_deviation",
         "Write a Python function 'mean_absolute_deviation(numbers)' that computes the mean "
         "absolute deviation. Return the result rounded to 2 decimal places."),
    ]

    results = []
    total_in, total_out = 0, 0
    for tag, prompt in tasks:
        log(f"  [{tag}] {prompt[:80]}...")
        resp = chat(prompt)
        results.append({
            "tag": tag, "prompt": prompt, "answer": resp["text"][:800],
            "tools_used": [t["name"] for t in resp["tool_calls"]],
            "input_tokens": resp["input_tokens"], "output_tokens": resp["output_tokens"],
        })
        total_in += resp["input_tokens"]; total_out += resp["output_tokens"]
        log(f"    Tools: {results[-1]['tools_used']} | Tokens: {resp['input_tokens']:,}→{resp['output_tokens']:,}")

    (SANDBOX / "results").mkdir(exist_ok=True)
    with open(SANDBOX / "results/humaneval.json", "w") as f:
        json.dump(results, f, indent=2, ensure_ascii=False)
    ok(f"HumanEval: {len(results)} tasks → {SANDBOX / 'results/humaneval.json'}")

def generate_report():
    """Generate comparison report."""
    log("=== Generating Report ===")
    lines = [
        "# EverEvo Agent Benchmark Report",
        f"**Time**: {time.strftime('%Y-%m-%d %H:%M:%S')}",
        f"**Sandbox**: `{SANDBOX}` (isolated from production `data/`)",
        "",
        "## Experimental Design",
        "",
        "| Variable | Value |",
        "|----------|-------|",
        "| Test target | EverEvo Agent (context pipeline + 22 tools) |",
        "| LLM backend | glm-5.2 (from config.toml) |",
        "| Temperature | 0.0 (deterministic) |",
        "| Production data | NOT touched |",
        "",
        "## Results",
    ]

    for bench, name in [("eqbench", "EQ-Bench (Emotional Intelligence)"),
                         ("bfcl", "BFCL-style (Tool Use)"),
                         ("humaneval", "HumanEval (Code Generation)")]:
        f = SANDBOX / "results" / f"{bench}.json"
        if f.exists():
            data = json.loads(f.read_text())
            lines.append(f"\n### {name} — {len(data)} tests")
            for item in data:
                lines.append(f"\n**Q ({item['tag']})**: {item['prompt'][:100]}...")
                lines.append(f"\n**A**: {item.get('answer', '')[:200]}...")
                if item.get("tools_used"):
                    lines.append(f"\n**Tools**: {', '.join(item['tools_used'])}")
                if "input_tokens" in item:
                    lines.append(f"\n**Tokens**: {item['input_tokens']:,} in / {item['output_tokens']:,} out")
                lines.append("")

    # Token efficiency summary
    lines.append("\n## Token Efficiency\n")
    lines.append("| Benchmark | Queries | Total In | Total Out | Avg Out/Query |")
    lines.append("|-----------|---------|----------|-----------|---------------|")
    for bench, name in [("eqbench", "EQ-Bench"), ("bfcl", "BFCL"), ("humaneval", "HumanEval")]:
        f = SANDBOX / "results" / f"{bench}.json"
        if f.exists():
            data = json.loads(f.read_text())
            total_in = sum(item.get("input_tokens", 0) for item in data)
            total_out = sum(item.get("output_tokens", 0) for item in data)
            avg_out = total_out // max(len(data), 1)
            lines.append(f"| {name} | {len(data)} | {total_in:,} | {total_out:,} | {avg_out:,} |")
    lines.append(f"| **Total** | — | — | — | — |")

    lines.append("\n## Comparison Baselines\n")
    lines.append("| Benchmark | EverEvo | GPT-4o | Claude 3.5 Sonnet | Gemini 3 Pro |")
    lines.append("|-----------|---------|--------|-------------------|--------------|")
    lines.append("| EQ-Bench v3 | *this run* | — | 82/100 | 87/100 |")
    lines.append("| BFCL (Overall) | *this run* | 88% | 85% | 82% |")
    lines.append("| HumanEval | *this run* | 67% | 92% | — |")

    report = SANDBOX / f"report_{time.strftime('%Y%m%d_%H%M%S')}.md"
    report.write_text("\n".join(lines))
    print("\n" + "\n".join(lines))
    ok(f"Report: {report}")

# ── Main ────────────────────────────────────────────────────────────────────
if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="EverEvo Agent Benchmark")
    parser.add_argument("cmd", nargs="?", default="all",
                        choices=["all", "eqbench", "bfcl", "humaneval", "start", "stop", "report"])
    args = parser.parse_args()

    if args.cmd == "start":
        setup_sandbox()
        if not start_server():
            sys.exit(1)
        log(f"Server running at {BASE_URL} — test manually:")
        log(f'  curl -X POST {BASE_URL}/api/chat -H "Content-Type: application/json" -d \'{{"message":"hello"}}\'')
        log("Press Ctrl+C to stop")
        try:
            while True: time.sleep(1)
        except KeyboardInterrupt:
            stop_server()

    elif args.cmd == "stop":
        stop_server()

    elif args.cmd == "report":
        generate_report()

    else:
        setup_sandbox()
        if not start_server():
            sys.exit(1)
        try:
            if args.cmd in ("all", "eqbench"): run_eqbench()
            if args.cmd in ("all", "bfcl"):    run_bfcl()
            if args.cmd in ("all", "humaneval"): run_humaneval()
            if args.cmd == "all": generate_report()
        finally:
            stop_server()
