#!/usr/bin/env python3
"""Extract wrong (exact_match=False) task_ids from a GAIA checkpoint.

Usage:
    python scripts/gaia_wrong_ids.py <checkpoint.jsonl> [--all]

Prints comma-separated task_ids of questions whose exact_match is False
(primary rows only — followup re-prompt rows are skipped). With `--all`,
prints the exact task_ids of every question instead.

Designed for the subset re-run flow: feed the output to
`gaia_bench.py --level all --ids <ids>` to re-run ONLY the wrong questions
against a freshly-built binary (the user's "run the wrong ones once" flow).
"""
import argparse
import json
import sys


def load_rows(path):
    rows = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def main():
    p = argparse.ArgumentParser()
    p.add_argument("checkpoint", help="path to checkpoint_*.jsonl")
    p.add_argument("--all", action="store_true",
                   help="print every task_id (not just the wrong ones)")
    args = p.parse_args()

    rows = load_rows(args.checkpoint)
    primary = [r for r in rows if not r.get("is_followup")]
    if args.all:
        ids = [r["task_id"] for r in primary]
    else:
        ids = [r["task_id"] for r in primary if not r.get("exact_match")]
    if not ids:
        print(f"(no {'questions' if args.all else 'wrong questions'})", file=sys.stderr)
        return 0
    print(",".join(ids))
    print(f"{len(ids)} ids", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
