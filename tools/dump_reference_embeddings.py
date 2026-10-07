#!/usr/bin/env python3
"""Capture reference embeddings for the chunked corpus and the vector queries.

Run with the *reference* interpreter (it owns `fastembed`):

    HF_HUB_OFFLINE=1 ~/.local/share/uv/tools/basic-memory/bin/python \
        tools/dump_reference_embeddings.py

The captured document is consumed by `FixtureEmbeddingProvider`
(`src/search/embedding.rs`), which lets the Rust search path replay the reference
vectors without the ONNX runtime:

* ``vectors``: chunk text → 384-dim vector (text-keyed because chunk keys embed
  internal row ids, which are not part of the compatibility contract)
* ``queries``: query text → vector

Everything runs offline: the model cache is the one the oracle harness copies into
its work dir (`~/.config/basic-memory/fastembed_cache`), and `HF_HUB_OFFLINE=1`
stops `huggingface_hub` from validating against the network.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_QUERIES = ["local index", "rust"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--chunks",
        type=Path,
        default=REPO_ROOT / "tests" / "golden" / "vector" / "chunks.json",
        help="captured chunk corpus",
    )
    parser.add_argument(
        "--model-cache",
        type=Path,
        default=Path.home() / ".config" / "basic-memory" / "fastembed_cache",
    )
    parser.add_argument("--model", default="BAAI/bge-small-en-v1.5")
    parser.add_argument(
        "--out",
        type=Path,
        default=REPO_ROOT / "tests" / "golden" / "vector" / "embeddings-reference.json",
    )
    parser.add_argument("--query", action="append", default=None)
    args = parser.parse_args()

    # The model cache is complete; keep `huggingface_hub` from revalidating online.
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")

    from fastembed import TextEmbedding

    corpus = json.loads(args.chunks.read_text())
    chunks = corpus["chunks"]
    texts = [chunk["chunk_text"] for chunk in chunks]
    queries = args.query or DEFAULT_QUERIES

    model = TextEmbedding(args.model, cache_dir=str(args.model_cache))
    documents = [list(map(float, vector)) for vector in model.embed(texts)]
    query_vectors = [
        list(map(float, vector)) for vector in model.embed(queries)
    ]
    if len(documents) != len(texts):
        raise SystemExit("embedding count does not match the chunk corpus")

    # Duplicate chunk texts are fine: the model is deterministic, so they share a
    # vector and the JSON stays small.
    vectors = {text: vector for text, vector in zip(texts, documents)}
    payload = {
        "model": args.model,
        "dimensions": len(documents[0]) if documents else 0,
        "vectors": vectors,
        "queries": dict(zip(queries, query_vectors)),
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, ensure_ascii=False, indent=1) + "\n")
    print(
        f"wrote {args.out} ({len(vectors)} chunk vectors, "
        f"{len(query_vectors)} queries, dim {payload['dimensions']})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
