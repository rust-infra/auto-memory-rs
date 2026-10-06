#!/usr/bin/env python3
"""Dump reference semantic chunks and embeddings for the oracle project.

Run with the reference interpreter (needs the reference DB built by
`tools/export_reference.py` and the fastembed cache copied into its work dir):

    ~/.local/share/uv/tools/basic-memory/bin/python \
        tools/dump_reference_vectors.py --workdir /tmp/basic-memory-oracle-xxxx

Writes `tests/golden/vector/chunks.json` and `embeddings.json`. The model is the
pinned local ONNX model, so this runs offline.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sqlite3
from dataclasses import dataclass
from pathlib import Path


@dataclass
class SourceRow:
    """Minimal stand-in for the reference SemanticSourceRow protocol."""

    id: int
    type: str
    title: str | None
    permalink: str | None
    content_snippet: str | None
    category: str | None
    relation_type: str | None
    entity_id: int | None = None


def load_rows(db: Path, project_id: int) -> list[SourceRow]:
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    rows = [
        SourceRow(
            id=row["id"],
            type=row["type"],
            title=row["title"],
            permalink=row["permalink"],
            content_snippet=row["content_snippet"],
            category=row["category"],
            relation_type=row["relation_type"],
            entity_id=row["entity_id"],
        )
        for row in conn.execute(
            "SELECT id, type, title, permalink, content_snippet, category, relation_type, entity_id "
            "FROM search_index WHERE project_id = ? ORDER BY type, id",
            (project_id,),
        )
    ]
    conn.close()
    return rows


async def capture(workdir: Path, out: Path, queries: list[str], no_embeddings: bool = False) -> None:
    from basic_memory.repository.fastembed_provider import FastEmbedEmbeddingProvider
    from basic_memory.repository.semantic_chunking import (
        build_entity_fingerprint,
        build_vector_chunk_records,
    )

    config_dir = workdir / "config"
    db = config_dir / "memory.db"
    cache = workdir / "fastembed_cache"
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    project_id = conn.execute("SELECT id FROM project WHERE name = 'oracle'").fetchone()[0]
    conn.close()

    rows = load_rows(db, project_id)
    build = build_vector_chunk_records(rows)
    records = build.records

    by_entity: dict[int, list[dict]] = {}
    entity_of: dict[str, int] = {}
    row_by_key: dict[str, dict] = {}
    for row in rows:
        entity_of[f"{row.type}:{row.id}"] = row.entity_id or row.id
        row_by_key[f"{row.type}:{row.id}"] = {
            "permalink": row.permalink,
            "file_path": None,
        }
    for record in records:
        prefix = ":".join(record["chunk_key"].split(":")[:2])
        entity_id = entity_of.get(prefix)
        if entity_id is not None:
            by_entity.setdefault(entity_id, []).append(record)
    fingerprints = {
        str(entity_id): build_entity_fingerprint(chunk_records)
        for entity_id, chunk_records in by_entity.items()
    }

    out.mkdir(parents=True, exist_ok=True)
    if no_embeddings:
        (out / "chunks.json").write_text(
            json.dumps(
                {
                    "model": "BAAI/bge-small-en-v1.5",
                    "dimensions": 384,
                    "duplicate_chunk_keys": build.duplicate_chunk_keys,
                    "chunks": [
                        {
                            **record,
                            "permalink": row_by_key.get(prefix_key := ":".join(record["chunk_key"].split(":")[:2]), {}).get("permalink"),
                            "file_path": row_by_key.get(prefix_key, {}).get("file_path"),
                            "entity_id": entity_of.get(":".join(record["chunk_key"].split(":")[:2])),
                            "entity_fingerprint": fingerprints.get(
                                str(entity_of.get(":".join(record["chunk_key"].split(":")[:2])))
                            ),
                        }
                        for record in records
                    ],
                },
                ensure_ascii=False,
                indent=2,
            )
            + "\n"
        )
        print(f"wrote {len(records)} chunks (embeddings skipped) to {out}")
        return

    provider = FastEmbedEmbeddingProvider(model_name="bge-small-en-v1.5", cache_dir=str(cache))
    vectors = await provider.embed_documents([record["chunk_text"] for record in records])
    query_vectors = {query: await provider.embed_query(query) for query in queries}

    (out / "chunks.json").write_text(
        json.dumps(
            {
                "model": "BAAI/bge-small-en-v1.5",
                "dimensions": provider.dimensions,
                "duplicate_chunk_keys": build.duplicate_chunk_keys,
                "chunks": [
                    {
                        **record,
                        "entity_id": entity_of.get(":".join(record["chunk_key"].split(":")[:2])),
                        "entity_fingerprint": fingerprints.get(
                            str(entity_of.get(":".join(record["chunk_key"].split(":")[:2])))
                        ),
                    }
                    for record in records
                ],
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n"
    )
    (out / "embeddings.json").write_text(
        json.dumps(
            {
                "model": "BAAI/bge-small-en-v1.5",
                "dimensions": provider.dimensions,
                "vectors": {
                    record["chunk_key"]: [round(value, 6) for value in vector]
                    for record, vector in zip(records, vectors)
                },
                "queries": {
                    query: [round(value, 6) for value in vector]
                    for query, vector in query_vectors.items()
                },
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n"
    )
    print(f"wrote {len(records)} chunks and {len(query_vectors)} query vectors to {out}")


def main() -> int:
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workdir", type=Path, required=True)
    parser.add_argument("--out", type=Path, default=repo / "tests" / "golden" / "vector")
    parser.add_argument("--query", action="append", default=None)
    parser.add_argument("--no-embeddings", action="store_true")
    args = parser.parse_args()
    queries = args.query or ["local index", "rust"]
    asyncio.run(
        capture(args.workdir.resolve(), args.out.resolve(), queries, args.no_embeddings)
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
