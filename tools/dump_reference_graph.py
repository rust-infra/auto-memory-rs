#!/usr/bin/env python3
"""Dump the reference graph rows and its `find_related` traversal.

Run with the *reference* interpreter against an oracle work dir:

    ~/.local/share/uv/tools/basic-memory/bin/python \
        tools/dump_reference_graph.py --db /tmp/basic-memory-oracle-XXXX/config/memory.db

Two artifacts are produced:

* ``--graph-out``: the raw ``entity`` / ``relation`` / ``observation`` rows including
  their ids, so a Rust test can load the *reference* id assignment into its own
  index and replay the traversal.
* ``--related-out``: the rows the reference ``ContextService`` traversal returns for
  a fixed set of cases, in order.

The traversal order and the ``max_related`` truncation depend on those ids, and the
reference assigns entity ids while indexing files concurrently, so two oracle runs
produce different id orders. Replaying the ported query against the dumped rows is
therefore the only way to prove the SQL port without depending on indexing order.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
from datetime import datetime, timedelta
from pathlib import Path

# Cases mirror the golden context captures; `since` matches the CLI's `7d` default.
CASES: list[dict] = [
    {"id": "alpha-depth1", "permalink": "oracle/projects/alpha", "depth": 1},
    {"id": "relations-depth1", "permalink": "oracle/notes/relations", "depth": 1},
    {"id": "relations-depth2", "permalink": "oracle/notes/relations", "depth": 2},
]


def dump_graph(conn: sqlite3.Connection) -> dict:
    entities = [
        {
            "id": row[0],
            "external_id": row[1],
            "project_id": row[2],
            "title": row[3],
            "note_type": row[4],
            "permalink": row[5],
            "file_path": row[6],
            "created_at": row[7],
            "updated_at": row[8],
        }
        for row in conn.execute(
            "SELECT id, external_id, project_id, title, note_type, permalink, file_path,"
            " created_at, updated_at FROM entity ORDER BY id"
        )
    ]
    relations = [
        {
            "id": row[0],
            "project_id": row[1],
            "from_id": row[2],
            "to_id": row[3],
            "to_name": row[4],
            "relation_type": row[5],
        }
        for row in conn.execute(
            "SELECT id, project_id, from_id, to_id, to_name, relation_type FROM relation"
            " ORDER BY id"
        )
    ]
    observations = [
        {
            "id": row[0],
            "project_id": row[1],
            "entity_id": row[2],
            "category": row[3],
            "content": row[4],
        }
        for row in conn.execute(
            "SELECT id, project_id, entity_id, category, content FROM observation ORDER BY id"
        )
    ]
    return {"entities": entities, "relations": relations, "observations": observations}


def related_query(seed_id: int, project_id: int, depth: int, max_related: int, since: str) -> str:
    """Render the reference SQLite traversal with its bound parameters inlined."""
    from basic_memory.services.context_service import ContextService

    statement = ContextService._build_sqlite_query(
        None,
        str(seed_id),
        "AND e.created_at >= :since_date",
        "AND e.project_id = :project_id",
        "",
        "AND e_from.created_at >= :since_date",
        "AND e_from.project_id = r.project_id",
        "AND eg.relation_date >= :since_date",
        "('entity', %d)" % seed_id,
    )
    return (
        str(statement)
        .replace(":max_depth", str(depth * 2))
        .replace(":max_results", str(max_related))
        .replace(":project_id", str(project_id))
        .replace(":since_date", repr(since))
    )


def dump_related(db_path: Path, graph: dict) -> dict:
    entities = {entity["permalink"]: entity["id"] for entity in graph["entities"]}
    since = (datetime.now().astimezone() - timedelta(days=7)).isoformat()
    project_id = graph["entities"][0]["project_id"]

    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    cases = []
    for case in CASES:
        seed_id = entities[case["permalink"]]
        sql = related_query(seed_id, project_id, case["depth"], 10, since)
        rows = conn.execute(sql).fetchall()
        cases.append(
            {
                "id": case["id"],
                "seed_id": seed_id,
                "depth": case["depth"],
                "max_related": 10,
                "rows": [
                    {
                        "type": row[0],
                        "id": row[1],
                        "title": row[2],
                        "permalink": row[3],
                        "file_path": row[4],
                        "from_id": row[5],
                        "to_id": row[6],
                        "relation_type": row[7],
                        "to_name": row[8],
                        "depth": row[12],
                        "root_id": row[13],
                    }
                    for row in rows
                ],
            }
        )
    conn.close()
    return {"since_days": 7, "cases": cases}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    repo = Path(__file__).resolve().parents[1]
    parser.add_argument("--db", type=Path, required=True, help="reference memory.db")
    parser.add_argument(
        "--graph-out",
        type=Path,
        default=repo / "tests" / "golden" / "index" / "graph-rows.json",
    )
    parser.add_argument(
        "--related-out",
        type=Path,
        default=repo / "tests" / "golden" / "context" / "find-related.json",
    )
    args = parser.parse_args()

    conn = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    graph = dump_graph(conn)
    conn.close()
    related = dump_related(args.db, graph)

    for path, payload in ((args.graph_out, graph), (args.related_out, related)):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n")
        print(f"wrote {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
