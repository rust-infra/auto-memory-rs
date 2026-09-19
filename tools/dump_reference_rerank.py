#!/usr/bin/env python3
"""Capture reference searches with the cross-encoder reranker enabled.

The reranker is off by default (`reranker_enabled=False`), so the default corpus never
exercises it. With `semantic_search_enabled` + `reranker_enabled` the vector and hybrid
legs rescore their top `reranker_candidates` rows with a fastembed cross-encoder
(`jinaai/jina-reranker-v1-tiny-en`), replace those rows' scores with the squashed
relevance, and demote the untouched tail below the reranked floor.

This harness runs the same hermetic setup as `export_reference.py` with both switches on
and writes `tests/golden/search/rerank-*.json`. It needs the model cache (the reranker is
downloaded on first use) and the reference CLI, so run it with escalation.
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import export_reference as oracle  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT = REPO_ROOT / "tests" / "golden" / "search"
DEFAULT_FIXTURES = REPO_ROOT / "tests" / "fixtures" / "vault"
DEFAULT_MODEL_CACHE = Path.home() / ".config" / "basic-memory" / "fastembed_cache"

RERANK_CASES: list[dict] = [
    {"id": "rerank-vector-rust", "query": "rust", "extra": ["--vector"]},
    {"id": "rerank-hybrid-rust", "query": "rust", "extra": ["--hybrid"]},
    {"id": "rerank-vector-local-index", "query": "local index", "extra": ["--vector"]},
    {"id": "rerank-vector-rust-type-note", "query": "rust",
     "extra": ["--vector", "--type", "note"]},
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--model-cache", type=Path, default=DEFAULT_MODEL_CACHE)
    parser.add_argument("--work-dir", type=Path, default=None)
    parser.add_argument("--keep-workdir", action="store_true")
    args = parser.parse_args()

    fixtures = args.fixtures.resolve()
    model_cache = args.model_cache
    if not model_cache.is_dir():
        print(f"model cache missing at {model_cache}", file=sys.stderr)
        return 2
    if shutil.which("basic-memory") is None:
        print("basic-memory CLI not found on PATH", file=sys.stderr)
        return 2

    if args.work_dir:
        work = args.work_dir.resolve()
        if work.exists():
            shutil.rmtree(work)
        work.mkdir(parents=True)
    else:
        work = Path(tempfile.mkdtemp(prefix="basic-memory-rerank-oracle-"))

    vault = work / "vault"
    config_dir = work / "config"
    home = work / "home"
    config_dir.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    shutil.copytree(fixtures, vault)
    cache_dst = work / "fastembed_cache"
    shutil.copytree(model_cache, cache_dst)

    oracle.write_json(
        config_dir / "config.json",
        {
            "env": "user",
            "projects": {
                oracle.PROJECT: {
                    "path": str(vault),
                    "mode": "local",
                    "workspace_id": None,
                    "local_sync_path": None,
                    "bisync_initialized": False,
                    "last_sync": None,
                }
            },
            "default_project": oracle.PROJECT,
            "database_backend": "sqlite",
            "semantic_search_enabled": True,
            "semantic_embedding_cache_dir": str(cache_dst),
            # The whole point of this harness.
            "reranker_enabled": True,
            "default_search_type": "text",
            "auto_update": False,
            "logfire_enabled": False,
            "logfire_send_to_logfire": False,
            "cli_output_style": "plain",
        },
    )

    env = os.environ.copy()
    env["BASIC_MEMORY_CONFIG_DIR"] = str(config_dir)
    env["HOME"] = str(home)
    env["NO_COLOR"] = "1"
    env["TERM"] = "dumb"
    env.pop("BASIC_MEMORY_PROJECT", None)

    oracle.log("reindex --full --search")
    index = oracle.run(
        ["reindex", "--full", "--search", "--project", oracle.PROJECT], env, timeout=900
    )
    if index["exit_code"] != 0:
        print(index["stdout"][-4000:], file=sys.stderr)
        return 1
    oracle.log("reindex --embeddings")
    embeddings = oracle.run(
        ["reindex", "--embeddings", "--project", oracle.PROJECT], env, timeout=1800
    )
    if embeddings["exit_code"] != 0:
        print(embeddings["stdout"][-4000:], file=sys.stderr)
        return 1

    out = args.out.resolve()
    for case in RERANK_CASES:
        argv = ["tool", "search-notes", case["query"], *case["extra"], "--json",
                "--project", oracle.PROJECT, "--local"]
        oracle.log(" ".join(argv))
        result = oracle.run(argv, env, timeout=900)
        if result["exit_code"] != 0:
            print(result["stdout"][-2000:], file=sys.stderr)
            print(result["stderr"][-2000:], file=sys.stderr)
            return 1
        payload = oracle.parse_json_output(result)
        oracle.write_json(out / f"{case['id']}.json", oracle.canon_json(payload, work, out))

    oracle.log(f"wrote {len(RERANK_CASES)} rerank goldens to {out}")
    if args.keep_workdir:
        oracle.log(f"workdir kept at {work}")
    else:
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
