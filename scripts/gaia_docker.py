#!/usr/bin/env python3
"""
GAIA benchmark for EverEvo Agent — Docker-per-task execution.

Each GAIA task runs in a FRESH Docker container from the `everevo-gaia` image:
  - New container  → new empty /data → memory (facts/diary/KG/persona) is
    fully isolated between tasks (zero cross-question contamination).
  - No embedding models mounted → RAG disabled → zero domain-knowledge leakage.
  - Attachment mounted at /files/<name> → agent reads it via shell tool.
  - `EVEREVO_PERMISSION_LEVEL=fully_auto` baked into image → shell tool never
    waits for human confirmation inside an unattended container.
  - Each chat() call sends no session_id → fresh conversation per task.

Usage:
    python scripts/gaia_docker.py --level level1              # full L1 (53)
    python scripts/gaia_docker.py --level level1 --limit 5    # smoke
    python scripts/gaia_docker.py --use-sample --limit 3      # offline smoke
    python scripts/gaia_docker.py --level all                 # 165 questions

Image must be built first:
    bash scripts/build_linux_binary.sh
    docker build -t everevo-gaia scripts/gaia-docker/
"""

import argparse, atexit, json, shutil, signal, socket, subprocess, sys, tempfile, time
from pathlib import Path

# Fix Windows GBK encoding for emoji output
if sys.platform == "win32":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")

# Reuse dataset loading, scoring, and prompt constants from gaia_bench.py
from gaia_bench import (
    load_gaia_dataset, score_answer, normalize,
    TOOL_ENFORCEMENT, AGENT_TECHNIQUE_HINT,
    RESULTS_DIR, log, ok, fail,
)

WS_ROOT = Path(__file__).resolve().parent.parent
IMAGE = "everevo-gaia"
CONTAINER_PORT = 13456
# Per-task time budget. GAIA multi-step tool use can be slow on a small model.
CHAT_TIMEOUT = 600
# How long a fresh container may take to boot the server.
BOOT_TIMEOUT = 120


# ---------------------------------------------------------------------------
# Docker container lifecycle
# ---------------------------------------------------------------------------
_STARTED: set = set()  # container ids started this run (for hard-kill cleanup)


def _docker(args: list, timeout: int = 60) -> subprocess.CompletedProcess | None:
    """Run a docker command. Returns None on timeout or missing docker CLI so
    callers can treat it as a daemon failure instead of crashing the run."""
    try:
        return subprocess.run(["docker", *args], capture_output=True,
                              text=True, timeout=timeout)
    except (subprocess.TimeoutExpired, FileNotFoundError) as e:
        fail(f"docker {' '.join(args[:2])} failed: {e}")
        return None


def _free_port() -> int:
    """Allocate a random free host port."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def start_container(attachment: Path | None, attach_name: str | None):
    """Start a fresh everevo-gaia container.

    Returns (cid, host_port, tmpdir) or None on failure. `tmpdir` is the
    per-task mount dir the caller's finally must remove (None when the task has
    no attachment). Only the task's OWN attachment file is mounted at /files:ro
    — never the whole HF validation dir, which would expose every other task's
    attachments to this container (cross-task contamination).
    """
    tmpdir = None
    if attachment and attachment.exists():
        tmpdir = tempfile.mkdtemp(prefix="everevo-gaia-")
        shutil.copy2(attachment, Path(tmpdir) / attachment.name)
        file_mount = ["-v", f"{tmpdir}:/files:ro"]
    else:
        # No attachment → still give the agent an (empty) /files mount point.
        file_mount = ["--tmpfs", "/files"]

    # Retry on free-port TOCTOU: the ephemeral port from _free_port() can be
    # grabbed by another process before docker run publishes it (→ "port is
    # already allocated").
    for _ in range(3):
        host_port = _free_port()
        cmd = ["docker", "run", "--rm", "-d",
               "-p", f"127.0.0.1:{host_port}:{CONTAINER_PORT}",
               *file_mount, IMAGE]
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        except (subprocess.TimeoutExpired, FileNotFoundError) as e:
            fail(f"docker run failed: {e}")
            break
        if proc.returncode == 0:
            cid = proc.stdout.strip()
            _STARTED.add(cid)
            return cid, host_port, tmpdir
        if "port is already allocated" in proc.stderr:
            fail(f"host port {host_port} raced away — retrying with a fresh port")
            continue
        fail(f"docker run failed: {proc.stderr.strip()}")
        break

    if tmpdir:
        shutil.rmtree(tmpdir, ignore_errors=True)
    return None


def stop_container(cid: str) -> None:
    """Stop + remove a container. Best-effort: a hanging `docker stop` falls
    back to `docker kill` so the host port is always released and nothing
    leaks across a multi-question run."""
    _STARTED.discard(cid)
    result = _docker(["stop", "-t", "15", cid], timeout=40)
    if result is None or result.returncode != 0:
        _docker(["kill", cid], timeout=20)  # stop hung/failed → force kill
    _docker(["rm", "-f", cid], timeout=20)  # release the host port immediately


def _cleanup_all_containers() -> None:
    """Best-effort cleanup for hard kills (SIGTERM/atexit). Note: a SIGKILL /
    OOM-kill of the runner cannot run this — one leaked container may remain."""
    for cid in list(_STARTED):
        _docker(["rm", "-f", cid], timeout=20)


atexit.register(_cleanup_all_containers)


def _handle_sigterm(signum, frame):
    _cleanup_all_containers()
    raise SystemExit(130)


try:
    signal.signal(signal.SIGTERM, _handle_sigterm)
except (ValueError, OSError):
    pass  # not the main thread → per-task finally still cleans up


def wait_ready(host_port: int, cid: str) -> bool:
    """Poll /api/health until 200 or timeout. Returns True when ready."""
    import requests as req
    base = f"http://127.0.0.1:{host_port}"
    deadline = time.time() + BOOT_TIMEOUT
    while time.time() < deadline:
        # If the container died, fail fast instead of polling to timeout.
        st = _docker(["inspect", "-f", "{{.State.Running}}", cid], timeout=20)
        if st is None:
            # Daemon hiccup — keep polling (deadline still bounds the wait).
            pass
        elif st.returncode == 0 and st.stdout.strip() != "true":
            logs = _docker(["logs", "--tail", "20", cid], timeout=20)
            fail(f"Container exited early:\n{logs.stdout if logs else '(no logs)'}")
            return False
        try:
            r = req.get(f"{base}/api/health", timeout=3)
            if r.status_code == 200:
                ok(f"Container ready (took {int(time.time() - (deadline - BOOT_TIMEOUT))}s)")
                return True
        except Exception:
            pass
        time.sleep(2)
    fail("Container health check timed out")
    _docker(["logs", "--tail", "30", cid], timeout=20)
    return False


# ---------------------------------------------------------------------------
# Prompt rewriting: host attachment paths → container /files paths
# ---------------------------------------------------------------------------
def container_prompt(q: dict) -> str:
    """Return the task prompt with attachment paths rewritten for the container.

    gaia_bench.load_gaia_dataset appends `The file is at: {host_path}` (the HF
    cache path on the host) to the Question. Inside the container that path
    doesn't exist — the attachment is mounted at /files/<file_name>. Rewrite
    the path so the agent's shell tool can read it.
    """
    prompt = q["Question"]
    if q.get("attachment") and q.get("file_name"):
        # Replace ONLY the harness-appended host path — an unanchored regex
        # would also rewrite any coincidental "The file is at: ..." line inside
        # the question body. q['attachment'] is exactly the host path that
        # gaia_bench.load_gaia_dataset appended.
        prompt = prompt.replace(
            f"The file is at: {q['attachment']}",
            f"The file is at: /files/{q['file_name']}",
        )
    return prompt


# ---------------------------------------------------------------------------
# Chat (same SSE parsing as gaia_bench, but port is per-task)
# ---------------------------------------------------------------------------
def chat(host_port: int, message: str, timeout: int = CHAT_TIMEOUT,
         enforce_tools: bool = True) -> dict:
    """Send message to a task container's chat API, collect SSE response."""
    import requests as req
    base = f"http://127.0.0.1:{host_port}"

    result = {"text": "", "tool_calls": [], "thinking": "",
              "input_tokens": 0, "output_tokens": 0, "error": None}
    full_message = message + AGENT_TECHNIQUE_HINT if not enforce_tools \
        else TOOL_ENFORCEMENT + message + AGENT_TECHNIQUE_HINT

    try:
        r = req.post(f"{base}/api/chat",
                     json={"message": full_message}, timeout=timeout, stream=True)
        r.raise_for_status()

        event = ""
        for raw_line in r.iter_lines(decode_unicode=True):
            if raw_line is None:
                continue
            line = raw_line.strip()
            if line.startswith("event: "):
                event = line[7:].strip()
            elif line.startswith("data: "):
                data_str = line[6:]
                try:
                    d = json.loads(data_str)
                except json.JSONDecodeError:
                    # Server `error` events carry a plain-string payload (not
                    # JSON). Capture it instead of silently dropping it —
                    # otherwise an infra failure (bad key, API down) scores as
                    # a wrong answer with error=None across the whole run.
                    if event == "error":
                        result["error"] = data_str[:500]
                    continue

                if event == "content_block_delta":
                    delta = d.get("delta", {})
                    delta_type = delta.get("type", "")
                    if delta_type == "text_delta":
                        result["text"] += delta.get("text", "")
                    elif delta_type == "thinking_delta":
                        result["thinking"] += delta.get("thinking", "")
                    elif delta_type == "input_json_delta":
                        # Tool args arrive in input_json_delta (block-start
                        # carries only an empty "input": {}), so accumulate.
                        if result["tool_calls"]:
                            result["tool_calls"][-1]["arguments"] += delta.get("partial_json", "")

                elif event == "content_block_start":
                    cb = d.get("content_block", {})
                    if cb.get("type") == "tool_use":
                        result["tool_calls"].append({
                            "name": cb.get("name", "?"),
                            "arguments": str(cb.get("input", ""))[:200],
                        })

                elif event == "done":
                    result["input_tokens"] = d.get("input_tokens", 0)
                    result["output_tokens"] = d.get("output_tokens", 0)
                    return result

                elif event == "error":
                    result["error"] = data_str[:500]
        return result
    except Exception as e:
        result["error"] = str(e)
        return result


# ---------------------------------------------------------------------------
# Main benchmark
# ---------------------------------------------------------------------------
def run_benchmark(questions: list, limit: int = None):
    if limit:
        questions = questions[:limit]

    log(f"Running GAIA benchmark (Docker-per-task): {len(questions)} questions")
    log(f"Image: {IMAGE} | container port: {CONTAINER_PORT} | timeout: {CHAT_TIMEOUT}s")
    print("=" * 70)

    results = []
    total_in, total_out = 0, 0
    exact_score, substring_score = 0, 0

    for i, q in enumerate(questions):
        tid = q["task_id"]
        prompt = container_prompt(q)
        gt = q["Final answer"]

        print(f"\n[{i+1}/{len(questions)}] {tid} (L{q['Level']})")
        print(f"  Q: {prompt[:100]}...")

        attachment = Path(q["attachment"]) if q.get("attachment") else None
        attach_name = q.get("file_name") or None

        started = start_container(attachment, attach_name)
        if not started:
            results.append(_err_result(q, "container start failed"))
            continue
        cid, host_port, tmpdir = started

        try:
            if not wait_ready(host_port, cid):
                results.append(_err_result(q, "container health timeout"))
                continue

            sys.stdout.write(f"  ⏳ "); sys.stdout.flush()
            t0 = time.time()
            resp = chat(host_port, prompt)
            elapsed = time.time() - t0
        finally:
            stop_container(cid)
            if tmpdir:
                shutil.rmtree(tmpdir, ignore_errors=True)

        scoring = score_answer(resp["text"], gt)
        passed = scoring["exact_match"] or scoring["substring_match"]
        if scoring["exact_match"]:
            exact_score += 1; status = "✅ EXACT"
        elif scoring["substring_match"]:
            substring_score += 1; status = "🟡 SUBSTR"
        else:
            status = "❌ FAIL"

        tools_used = [t["name"] for t in resp["tool_calls"]]
        total_in += resp["input_tokens"]
        total_out += resp["output_tokens"]

        print(f"  {status} ({elapsed:.0f}s) | pred: {scoring['predicted'][:80]}")
        print(f"  GT: {gt} | tools: {tools_used} | "
              f"tok: {resp['input_tokens']}→{resp['output_tokens']}")

        results.append({
            "task_id": tid,
            "question": prompt,
            "level": q["Level"],
            "ground_truth": gt,
            "predicted": resp["text"][:500],
            "tools_used": tools_used,
            "pass": passed,
            "exact_match": scoring["exact_match"],
            "input_tokens": resp["input_tokens"],
            "output_tokens": resp["output_tokens"],
            "error": resp.get("error"),
        })

    _report(results, exact_score, substring_score, total_in, total_out)


def _err_result(q: dict, error: str) -> dict:
    return {
        "task_id": q["task_id"], "question": q["Question"],
        "level": q["Level"], "ground_truth": q["Final answer"],
        "predicted": "", "tools_used": [], "pass": False,
        "exact_match": False, "input_tokens": 0, "output_tokens": 0,
        "error": error,
    }


def _report(results, exact_score, substring_score, total_in, total_out):
    n = len(results)
    print("\n" + "=" * 70)
    print("GAIA Benchmark Results — EverEvo Agent (Docker per-task)")
    print("=" * 70)
    print(f"  Questions:           {n}")
    print(f"  Exact Match:         {exact_score}/{n} ({exact_score/n*100:.1f}%)")
    print(f"  Substring Match:     {substring_score}/{n} ({substring_score/n*100:.1f}%)")
    print(f"  Any Match:           {exact_score + substring_score}/{n} ({(exact_score + substring_score)/n*100:.1f}%)")
    print(f"  Total tokens in:     {total_in:,}")
    print(f"  Total tokens out:    {total_out:,}")
    print(f"  Avg tokens/query:    {total_out // max(n, 1):,}")

    for lvl in sorted(set(r["level"] for r in results)):
        lvl_results = [r for r in results if r["level"] == lvl]
        lvl_pass = sum(1 for r in lvl_results if r["pass"])
        print(f"  Level {lvl}:           {lvl_pass}/{len(lvl_results)} ({lvl_pass/len(lvl_results)*100:.1f}%)")

    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    ts = time.strftime("%Y%m%d_%H%M%S")
    out = RESULTS_DIR / f"gaia_results_{ts}.json"
    questions_with_tools = sum(1 for r in results if r["tools_used"])
    total_tool_calls = sum(len(r["tools_used"]) for r in results)
    with open(out, "w", encoding="utf-8") as f:
        json.dump({
            "config": {
                "benchmark": "GAIA",
                "agent": "EverEvo",
                "execution": "docker-per-task",
                "image": IMAGE,
                "temperature": 0.0,
                "scoring": "Exact Match + Tool Verification Required",
                "questions": n,
                "tool_enforcement": True,
                "memory_isolation": "fresh container + empty /data per task",
                "rag": "disabled (no embedding models mounted)",
            },
            "summary": {
                "exact_match": f"{exact_score}/{n}",
                "exact_match_pct": round(exact_score / n * 100, 1),
                "substring_match": f"{substring_score}/{n}",
                "any_match_pct": round((exact_score + substring_score) / n * 100, 1),
                "total_input_tokens": total_in,
                "total_output_tokens": total_out,
                "questions_using_tools": f"{questions_with_tools}/{n}",
                "total_tool_calls": total_tool_calls,
            },
            "results": results,
        }, f, indent=2, ensure_ascii=False)
    print(f"  Tool Usage:          {questions_with_tools}/{n} questions used tools ({total_tool_calls} total tool calls)")
    ok(f"Results saved: {out}")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="GAIA Docker-per-task benchmark")
    parser.add_argument("--level", default="level1",
                        choices=["level1", "level2", "level3", "all"],
                        help="GAIA difficulty level (default: level1)")
    parser.add_argument("--limit", type=int, default=None,
                        help="Max questions to run (default: all)")
    parser.add_argument("--use-sample", action="store_true",
                        help="Use built-in sample questions (no HF auth needed)")
    args = parser.parse_args()

    # --use-sample is the offline smoke path
    use_sample = args.use_sample or not any(
        p in sys.argv for p in ("--level", "level1", "level2", "level3", "all")
    )

    questions = load_gaia_dataset(use_sample=use_sample, level=args.level)
    if not questions:
        fail("No questions loaded")
        sys.exit(1)

    # Verify image exists before starting the first task.
    check = _docker(["image", "inspect", IMAGE], timeout=30)
    if check is None or check.returncode != 0:
        fail(f"Image '{IMAGE}' not found. Build it first:\n"
             f"  bash scripts/build_linux_binary.sh\n"
             f"  docker build -t {IMAGE} scripts/gaia-docker/")
        sys.exit(1)

    run_benchmark(questions, limit=args.limit)
