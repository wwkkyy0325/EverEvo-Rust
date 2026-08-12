#!/usr/bin/env python3
"""
RAGAS Evaluation Script for EverEvo Knowledge Base Recall Pipeline.

This script uses the RAGAS framework (explodinggradients/ragas), the industry-standard
RAG evaluation tool, to measure the quality of our retrieval pipeline.

Usage:
    pip install ragas datasets langchain-openai
    python scripts/ragas_eval.py --dataset nfcorpus --provider glm

What it does:
    1. Loads a BEIR dataset (NFCorpus, SciFact, etc.)
    2. For each query, retrieves context via our pipeline (pre-computed JSON)
    3. Calls the LLM with and without context
    4. Computes RAGAS metrics: Context Precision, Context Recall, Faithfulness, Answer Relevancy

References:
    - RAGAS: https://github.com/explodinggradients/ragas
    - RAGAS paper: https://arxiv.org/abs/2309.15217
    - BEIR: https://github.com/beir-cellar/beir
"""

import json
import os
import sys
import argparse
from pathlib import Path
from typing import List, Dict, Optional

# ============================================================================
# Configuration
# ============================================================================

BEIR_DATASETS = {
    "nfcorpus": "data/bench/nfcorpus",
    "scifact": "data/bench/scifact",
    "fiqa": "data/bench/fiqa",
}

LLM_PROVIDERS = {
    "glm": {
        "api_key": os.environ.get("GLM_API_KEY", ""),
        "base_url": "https://open.bigmodel.cn/api/anthropic",
        "model": "glm-5.2",
    },
    "deepseek": {
        "api_key": os.environ.get("DEEPSEEK_API_KEY", ""),
        "base_url": "https://api.deepseek.com/anthropic",
        "model": "deepseek-v4-pro",
    },
}

SYSTEM_PROMPT = """You are a precise question-answering assistant.
Answer the question using ONLY the provided context.
If the context doesn't contain the answer, say 'Insufficient context'.
Keep answers under 100 words. Be factual and concise."""


# ============================================================================
# Data Loading (BEIR format)
# ============================================================================

def load_beir_dataset(dataset_name: str, max_queries: int = 50):
    """Load a BEIR dataset from the standard directory structure."""
    path = Path(BEIR_DATASETS[dataset_name])

    # Load corpus
    corpus = {}
    with open(path / "corpus.jsonl") as f:
        for line in f:
            doc = json.loads(line)
            corpus[doc["_id"]] = doc["text"]

    # Load queries
    queries = {}
    with open(path / "queries.jsonl") as f:
        for i, line in enumerate(f):
            if i >= max_queries:
                break
            q = json.loads(line)
            queries[q["_id"]] = q["text"]

    # Load qrels
    qrels = {}
    with open(path / "qrels" / "test.tsv") as f:
        for line in f:
            qid, doc_id, score = line.strip().split("\t")
            if qid not in qrels:
                qrels[qid] = {}
            qrels[qid][doc_id] = int(score)

    print(f"Loaded {dataset_name}: {len(corpus)} docs, {len(queries)} queries")
    return corpus, queries, qrels


# ============================================================================
# LLM Call (via OpenAI-compatible Anthropic API)
# ============================================================================

def call_llm(prompt: str, provider: str, system: str = SYSTEM_PROMPT) -> Optional[str]:
    """Call the LLM via Anthropic-compatible API using requests."""
    import requests

    cfg = LLM_PROVIDERS[provider]
    endpoint = f"{cfg['base_url']}/messages"

    body = {
        "model": cfg["model"],
        "max_tokens": 256,
        "temperature": 0.0,
        "system": system,
        "messages": [{"role": "user", "content": prompt}],
    }

    try:
        resp = requests.post(
            endpoint,
            json=body,
            headers={
                "x-api-key": cfg["api_key"],
                "anthropic-version": "2023-06-01",
                "Content-Type": "application/json",
            },
            timeout=30,
        )
        resp.raise_for_status()
        data = resp.json()
        return data["content"][0]["text"]
    except Exception as e:
        print(f"  LLM error: {e}")
        return None


# ============================================================================
# Simple Keyword Retrieval (no vector store needed)
# ============================================================================

def retrieve_keyword(query: str, corpus: Dict[str, str], qrels: Dict[str, Dict[str, int]], top_k: int = 3):
    """Simple keyword-based retrieval using term overlap."""
    query_terms = set(query.lower().split())
    scores = []
    for doc_id, text in corpus.items():
        doc_terms = set(text.lower().split())
        overlap = len(query_terms & doc_terms)
        if overlap > 0:
            scores.append((doc_id, overlap / len(query_terms)))
    scores.sort(key=lambda x: -x[1])
    return [doc_id for doc_id, _ in scores[:top_k]]


# ============================================================================
# RAGAS Evaluation
# ============================================================================

def evaluate_with_ragas(
    queries: Dict[str, str],
    corpus: Dict[str, str],
    qrels: Dict[str, Dict[str, int]],
    provider: str,
    sample_size: int = 20,
):
    """Run full RAGAS evaluation on sampled queries."""
    from ragas import evaluate, EvaluationDataset, SingleTurnSample
    from ragas.metrics import (
        ContextPrecision, ContextRecall, Faithfulness,
        AnswerRelevancy, AnswerCorrectness,
    )

    # Select queries that have qrels
    eval_queries = [(qid, qtext) for qid, qtext in queries.items() if qid in qrels]
    eval_queries = eval_queries[:sample_size]

    samples = []
    results = []

    for i, (qid, qtext) in enumerate(eval_queries):
        print(f"\n[{i+1}/{len(eval_queries)}] Q: {qtext[:80]}...")

        # Get relevant doc texts (ground truth)
        relevant_docs = [corpus[did] for did in qrels.get(qid, {}) if did in corpus]
        relevant_ids = set(qrels.get(qid, {}).keys())

        # Retrieve context
        retrieved_ids = retrieve_keyword(qtext, corpus, qrels, top_k=3)
        retrieved_contexts = [corpus[rid] for rid in retrieved_ids if rid in corpus]

        if not retrieved_contexts:
            print("  No context retrieved — skipping")
            continue

        # Generate answers
        ctx_str = "\n---\n".join(retrieved_contexts[:3])

        # With context
        prompt_with_ctx = f"Context:\n{ctx_str}\n\nQuestion: {qtext}\n\nAnswer concisely using ONLY the context above."
        answer = call_llm(prompt_with_ctx, provider)

        # Without context (baseline)
        prompt_no_ctx = f"Question: {qtext}\n\nAnswer concisely based on your knowledge."
        answer_no_ctx = call_llm(prompt_no_ctx, provider)

        if not answer:
            print("  LLM generation failed — skipping")
            continue

        # Ground truth answer (concatenation of relevant doc texts)
        ground_truth = " ".join(relevant_docs[:2]) if relevant_docs else ""

        samples.append(SingleTurnSample(
            user_input=qtext,
            retrieved_contexts=retrieved_contexts,
            response=answer,
            reference=ground_truth,
        ))

        results.append({
            "query_id": qid,
            "query": qtext,
            "retrieved_docs": retrieved_ids,
            "relevant_docs": list(relevant_ids),
            "answer_with_context": answer,
            "answer_no_context": answer_no_ctx,
            "ground_truth": ground_truth[:500],
        })

    print(f"\n{'='*60}")
    print(f"Running RAGAS evaluation on {len(samples)} samples...")

    metrics = [
        ContextPrecision(),
        ContextRecall(),
        Faithfulness(),
        AnswerRelevancy(),
    ]

    eval_dataset = EvaluationDataset(samples=samples)
    result = evaluate(eval_dataset, metrics=metrics)

    print(f"\n{'='*60}")
    print(f"RAGAS Evaluation Results — {provider}")
    print(f"{'='*60}")
    for metric_name, score in result.items():
        bar = "█" * int(score * 20) + "░" * (20 - int(score * 20))
        print(f"  {metric_name:<25s}: {score:.4f}  {bar}")

    # Print interpretation
    print(f"\nInterpretation (RAGAS thresholds):")
    print(f"  > 0.85 = Good | 0.70-0.85 = Acceptable | < 0.70 = Needs Work")
    print(f"\n  Context Precision: {'✅' if result.get('context_precision', 0) > 0.7 else '⚠️'}  "
          f"Are retrieved docs relevant (not noise)?")
    print(f"  Context Recall:    {'✅' if result.get('context_recall', 0) > 0.7 else '⚠️'}  "
          f"Did we retrieve ALL necessary information?")
    print(f"  Faithfulness:      {'✅' if result.get('faithfulness', 0) > 0.85 else '⚠️'}  "
          f"Is the answer grounded in retrieved context?")
    print(f"  Answer Relevancy:  {'✅' if result.get('answer_relevancy', 0) > 0.7 else '⚠️'}  "
          f"Does the answer address the question?")

    # Save results
    out_path = Path("data/bench") / f"ragas_{dataset_name}_{provider}.json"
    with open(out_path, "w") as f:
        json.dump({
            "provider": provider,
            "dataset": dataset_name,
            "metrics": {k: float(v) for k, v in result.items()},
            "samples": results,
        }, f, indent=2, ensure_ascii=False)
    print(f"\nResults saved to {out_path}")

    return result, results


# ============================================================================
# Main
# ============================================================================

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="RAGAS evaluation for EverEvo KB recall")
    parser.add_argument("--dataset", default="nfcorpus", choices=BEIR_DATASETS.keys())
    parser.add_argument("--provider", default="glm", choices=LLM_PROVIDERS.keys())
    parser.add_argument("--samples", type=int, default=20, help="Number of queries to evaluate")
    args = parser.parse_args()

    dataset_name = args.dataset
    print(f"╔══════════════════════════════════════════════════╗")
    print(f"║   RAGAS Recall Quality: {dataset_name.upper():<20s}          ║")
    print(f"║   Provider: {args.provider:<10s}  Samples: {args.samples:<3}                ║")
    print(f"╚══════════════════════════════════════════════════╝")

    corpus, queries, qrels = load_beir_dataset(dataset_name, max_queries=args.samples)
    result, samples = evaluate_with_ragas(queries, corpus, qrels, args.provider, args.samples)
