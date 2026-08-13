#!/usr/bin/env python
"""Export a GAIA run's checkpoint into the official submission JSONL format
   ({"task_id", "model_answer", "reasoning_trace"}) and score it with the
   official quasi-exact scorer (scripts/官网校验.py).

The checkpoint written by gaia_bench.py stores per-question: task_id,
ground_truth, predicted (full response text), thinking (reasoning trace).
This script:
  1. re-extracts model_answer with the harness's official extractor,
  2. writes submission_<ts>.jsonl in the GAIA leaderboard format,
  3. scores each model_answer against ground_truth using scripts/官网校验.py,
  4. prints per-question PASS/FAIL + overall score and saves the score report.

Usage:
  python scripts/export_submission.py --checkpoint data/bench/gaia-results/checkpoint_*.jsonl
  python scripts/export_submission.py --results data/bench/gaia-results/gaia_results_*.json
"""
import argparse
import importlib.util
import json
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).parent))

# gaia_bench's argparse block is guarded by `if __name__ == "__main__"`, so
# importing it here is side-effect-free (SCORING_MODE defaults to "official").
from gaia_bench import extract_final_answer, score_answer

HERE = pathlib.Path(__file__).parent
OFFICIAL_SCORER = HERE / "官网校验.py"


def load_official_scorer():
    """Import scripts/官网校验.py (non-ASCII filename) via importlib."""
    spec = importlib.util.spec_from_file_location("official_scorer", OFFICIAL_SCORER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def iter_records(checkpoint=None, results=None):
    if checkpoint:
        for line in checkpoint.open(encoding="utf-8"):
            line = line.strip()
            if line:
                yield json.loads(line)
    elif results:
        data = json.loads(results.open(encoding="utf-8"))
        yield from data.get("results", data) if isinstance(data, dict) else data
    else:
        sys.exit("error: pass --checkpoint or --results")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--checkpoint", type=pathlib.Path, default=None)
    ap.add_argument("--results", type=pathlib.Path, default=None)
    ap.add_argument("--out", type=pathlib.Path, default=None,
                    help="submission JSONL output path (default: "
                         "data/bench/gaia-results/submission_<ts>.jsonl)")
    args = ap.parse_args()
    if not args.checkpoint and not args.results:
        ap.error("need --checkpoint or --results")

    official = load_official_scorer()

    if args.out:
        out_path = args.out
    else:
        ts = time.strftime("%Y%m%d_%H%M%S")
        out_path = HERE.parent / "data" / "bench" / "gaia-results" / f"submission_{ts}.jsonl"
    out_path.parent.mkdir(parents=True, exist_ok=True)

    score_report = {"scoring": "官网校验.py (official quasi-exact)", "submission": str(out_path),
                    "results": []}
    passed = 0
    total = 0
    with out_path.open("w", encoding="utf-8") as fout:
        for rec in iter_records(args.checkpoint, args.results):
            tid = rec.get("task_id", "?")
            gt = rec.get("ground_truth", "")
            predicted = rec.get("predicted", rec.get("model_answer", ""))
            thinking = rec.get("thinking") or rec.get("reasoning_trace") or ""

            # model_answer = harness's official-mode extraction (marker value,
            # last-line fallback, Phase-1a terminal-value recovery).
            model_answer = ""
            had_marker = False
            try:
                sc = score_answer(predicted, gt)
                model_answer = sc.get("predicted") or ""
            except Exception:
                model_answer, had_marker = extract_final_answer(predicted)

            rec_score = official.question_scorer(model_answer, gt)
            total += 1
            passed += 1 if rec_score else 0
            mark = "PASS" if rec_score else "FAIL"
            print(f"  {mark}  {tid[:8]}  model_answer={model_answer!r}  GT={gt!r}")

            fout.write(json.dumps({
                "task_id": tid,
                "model_answer": model_answer,
                "reasoning_trace": thinking,
            }, ensure_ascii=False) + "\n")

            score_report["results"].append({
                "task_id": tid, "model_answer": model_answer,
                "ground_truth": gt, "pass": rec_score,
                "had_marker": had_marker, "thinking_len": len(thinking),
            })

    score_report["passed"] = passed
    score_report["total"] = total
    score_report["accuracy"] = round(passed / total, 4) if total else 0.0

    report_path = out_path.with_name(f"submission_score_{time.strftime('%Y%m%d_%H%M%S')}.json")
    with report_path.open("w", encoding="utf-8") as f:
        json.dump(score_report, f, indent=2, ensure_ascii=False)

    print(f"\n  Submission: {out_path}")
    if total:
        print(f"  Score (官网校验.py official): {passed}/{total} = {passed / total:.1%}")
    else:
        print("  (no records)")
    print(f"  Report: {report_path}")


if __name__ == "__main__":
    main()
