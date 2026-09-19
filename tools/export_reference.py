#!/usr/bin/env python3
"""Export Basic Memory reference (golden) outputs using the pinned 0.23.2 oracle.

The harness is hermetic: it never touches the real ~/.config/basic-memory. It
creates a throwaway config dir, registers the fixture vault there, reindexes,
then captures parse/index/search/context outputs into tests/golden/.

Requires the reference CLI (`basic-memory`) on PATH. Vector/hybrid cases need a
fastembed model cache (default: ~/.config/basic-memory/fastembed_cache) which is
copied into the throwaway work dir so the run stays offline.

Usage:
    python3 tools/export_reference.py [--no-embeddings] [--keep-workdir]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FIXTURES = REPO_ROOT / "tests" / "fixtures" / "vault"
DEFAULT_OUT = REPO_ROOT / "tests" / "golden"
DEFAULT_MODEL_CACHE = Path.home() / ".config" / "basic-memory" / "fastembed_cache"
# Interpreter that owns the reference install; used for dumps that must import
# `basic_memory` directly (the graph replay golden needs the reference SQL builder).
DEFAULT_ORACLE_PYTHON = Path.home() / ".local/share/uv/tools/basic-memory/bin/python"
ORACLE_PYTHON = os.environ.get("BASIC_MEMORY_ORACLE_PYTHON", str(DEFAULT_ORACLE_PYTHON))
PROJECT = "oracle"

UUID_RE = re.compile(
    r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
)
TS_RE = re.compile(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?")

SEARCH_CASES: list[dict] = [
    {"id": "text-rust", "query": "rust"},
    {"id": "text-case-insensitive", "query": "RUST"},
    {"id": "text-phrase", "query": '"source of truth"'},
    {"id": "text-boolean", "query": "rust AND architecture"},
    {"id": "text-prefix", "query": "arch*"},
    {"id": "text-cjk", "query": "测试"},
    {"id": "text-no-results", "query": "zzzz-not-present"},
    {"id": "title-alpha", "query": None, "extra": ["--title", "Alpha"]},
    {"id": "permalink-glob", "query": None, "extra": ["--permalink", "projects/*"]},
    {"id": "tag-rust", "query": None, "extra": ["--tag", "rust"]},
    {"id": "type-project", "query": None, "extra": ["--type", "project"]},
    {"id": "entity-type-observation", "query": None,
     "extra": ["--entity-type", "observation", "--category", "decision"]},
    # A category filter without an explicit `--entity-type`: the reference scopes the
    # implicit default to observation rows, because categories only exist there.
    {"id": "category-decision-implicit", "query": "rust", "extra": ["--category", "decision"]},
    {"id": "entity-type-relation", "query": "alpha", "extra": ["--entity-type", "relation"]},
    {"id": "status-archived", "query": None, "extra": ["--status", "archived"]},
    # Absolute bounds keep the corpus deterministic: relative bounds depend on the
    # wall clock at capture *and* at replay.
    {"id": "after-date-absolute-rust", "query": "rust", "extra": ["--after_date", "2026-09-01"]},
    {"id": "after-date-future-rust", "query": "rust", "extra": ["--after_date", "2030-01-01"]},
    {"id": "metadata-status-active", "query": None, "extra": ["--meta", "status=active"]},
    {"id": "pagination-note-page2", "query": "note", "extra": ["--page", "2", "--page-size", "2"]},
]

VECTOR_CASES: list[dict] = [
    {"id": "hybrid-rust", "query": "rust", "extra": ["--hybrid"]},
    {"id": "vector-local-index", "query": "local index", "extra": ["--vector"]},
    # Filters on the semantic legs. The reference passes them to the FTS leg natively
    # and intersects the vector leg with a filter-only scan, so these cases pin both
    # halves of the filter contract.
    {"id": "vector-rust-type-note", "query": "rust", "extra": ["--vector", "--type", "note"]},
    {"id": "vector-rust-type-project", "query": "rust", "extra": ["--vector", "--type", "project"]},
    {"id": "hybrid-rust-type-project", "query": "rust", "extra": ["--hybrid", "--type", "project"]},
    {"id": "vector-rust-entity-observation", "query": "rust",
     "extra": ["--vector", "--entity-type", "observation"]},
    {"id": "vector-rust-tag-rust", "query": "rust", "extra": ["--vector", "--tag", "rust"]},
    {"id": "vector-rust-status-active", "query": "rust",
     "extra": ["--vector", "--status", "active"]},
    {"id": "vector-local-index-entity-only", "query": "local index",
     "extra": ["--vector", "--entity-type", "entity"]},
    {"id": "hybrid-rust-category-decision", "query": "rust",
     "extra": ["--hybrid", "--category", "decision"]},
]

CONTEXT_CASES: list[dict] = [
    {"id": "relations-depth1", "url": "memory://notes/relations", "extra": []},
    {"id": "relations-depth2", "url": "memory://notes/relations", "extra": ["--depth", "2"]},
    {"id": "alpha-depth1", "url": "memory://projects/alpha", "extra": []},
    {"id": "frontmatter-depth1", "url": "memory://notes/frontmatter", "extra": []},
    {"id": "duplicates-dup-b", "url": "memory://duplicates/dup-b/same-title", "extra": []},
    {"id": "cjk-depth1", "url": "memory://notes/cjk", "extra": []},
]

CONTEXT_TEXT_CASES: list[dict] = [
    {"id": "simple-depth1", "url": "memory://notes/simple", "extra": []},
    {"id": "relations-depth2", "url": "memory://notes/relations", "extra": ["--depth", "2"]},
]

ERROR_CASES: list[dict] = [
    {"id": "search-page-size-0", "argv": ["tool", "search-notes", "rust", "--page-size", "0", "--json"]},
    {"id": "context-page-size-0", "argv": ["tool", "build-context", "memory://notes/simple", "--page-size", "0", "--json"]},
    {"id": "context-depth-too-large", "argv": ["tool", "build-context", "memory://notes/simple", "--depth", "9", "--json"]},
]


def log(msg: str) -> None:
    print(f"[oracle] {msg}", flush=True)


def run(argv: list[str], env: dict, timeout: int = 600) -> dict:
    return run_program(["basic-memory", *argv], env, timeout)


def run_program(command: list[str], env: dict, timeout: int = 600) -> dict:
    """Run one command in the oracle environment and capture its output."""
    started = time.perf_counter()
    proc = subprocess.run(
        command,
        env=env,
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return {
        "argv": command,
        "exit_code": proc.returncode,
        "stdout": proc.stdout,
        "stderr": proc.stderr,
        "seconds": round(time.perf_counter() - started, 3),
    }


def canon_text(text: str, workdir: Path, out_dir: Path) -> str:
    text = text.replace(str(workdir), "<work>")
    text = text.replace(str(out_dir), "<golden>")
    text = text.replace(str(REPO_ROOT), "<repo>")
    text = UUID_RE.sub("<uuid>", text)
    text = TS_RE.sub("<timestamp>", text)
    return text


ID_KEYS = {
    "id",
    "entity_id",
    "observation_id",
    "relation_id",
    "from_entity_id",
    "to_entity_id",
    "from_id",
    "to_id",
}


def canon_json(value, workdir: Path, out_dir: Path):
    if isinstance(value, dict):
        return {
            k: (f"<{k}>" if k in ID_KEYS and isinstance(v, int) else canon_json(v, workdir, out_dir))
            for k, v in value.items()
        }
    if isinstance(value, list):
        return [canon_json(v, workdir, out_dir) for v in value]
    if isinstance(value, str):
        return canon_text(value, workdir, out_dir)
    return value


def write_json(path: Path, payload) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=False) + "\n")


def parse_json_output(run_result: dict):
    text = run_result["stdout"].strip()
    if not text:
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        # Some commands print a header line before JSON; find the first JSON value.
        for idx, ch in enumerate(text):
            if ch in "{[":
                try:
                    return json.loads(text[idx:])
                except json.JSONDecodeError:
                    continue
        return None


def dump_database(db_path: Path, project_id: int) -> dict:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    try:
        entities = [
            {
                "file_path": r["file_path"],
                "title": r["title"],
                "note_type": r["note_type"],
                "permalink": r["permalink"],
                "content_type": r["content_type"],
                "checksum": r["checksum"],
                "metadata": json.loads(r["entity_metadata"]) if r["entity_metadata"] else None,
            }
            for r in conn.execute(
                """
                SELECT file_path, title, note_type, permalink, content_type, checksum, entity_metadata
                FROM entity WHERE project_id = ? ORDER BY file_path
                """,
                (project_id,),
            )
        ]
        observations = [
            {
                "entity_permalink": r["permalink"],
                "entity_file_path": r["file_path"],
                "category": r["category"],
                "content": r["content"],
                "context": r["context"],
                "tags": json.loads(r["tags"]) if r["tags"] else [],
            }
            for r in conn.execute(
                """
                SELECT e.permalink AS permalink, e.file_path AS file_path, o.category, o.content,
                       o.context, o.tags
                FROM observation o JOIN entity e ON e.id = o.entity_id
                WHERE o.project_id = ?
                ORDER BY e.file_path, o.category, o.content
                """,
                (project_id,),
            )
        ]
        relations = [
            {
                "from_permalink": r["from_permalink"],
                "from_file_path": r["from_file_path"],
                "relation_type": r["relation_type"],
                "to_name": r["to_name"],
                "to_permalink": r["to_permalink"],
                "to_file_path": r["to_file_path"],
                "context": r["context"],
            }
            for r in conn.execute(
                """
                SELECT f.permalink AS from_permalink, f.file_path AS from_file_path,
                       rel.relation_type, rel.to_name, rel.context,
                       t.permalink AS to_permalink, t.file_path AS to_file_path
                FROM relation rel
                JOIN entity f ON f.id = rel.from_id
                LEFT JOIN entity t ON t.id = rel.to_id
                WHERE rel.project_id = ?
                ORDER BY f.file_path, rel.relation_type, rel.to_name
                """,
                (project_id,),
            )
        ]
        try:
            search_rows = [
                {
                    "type": r["type"],
                    "title": r["title"],
                    "permalink": r["permalink"],
                    "file_path": r["file_path"],
                    "category": r["category"],
                    "relation_type": r["relation_type"],
                    "content_stems": r["content_stems"],
                    "content_snippet": r["content_snippet"],
                }
                for r in conn.execute(
                    """
                    SELECT type, title, permalink, file_path, category, relation_type,
                           content_stems, content_snippet
                    FROM search_index WHERE project_id = ?
                    ORDER BY type, permalink, file_path, title
                    """,
                    (project_id,),
                )
            ]
        except sqlite3.OperationalError as exc:
            search_rows = [{"error": f"search_index unavailable: {exc}"}]
    finally:
        conn.close()
    return {
        "entities": entities,
        "observations": observations,
        "relations": relations,
        "search_index": search_rows,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--work-dir", type=Path, default=None)
    parser.add_argument("--model-cache", type=Path, default=DEFAULT_MODEL_CACHE)
    parser.add_argument("--no-embeddings", action="store_true")
    parser.add_argument("--keep-workdir", action="store_true")
    args = parser.parse_args()

    fixtures = args.fixtures.resolve()
    if not fixtures.is_dir():
        print(f"fixture vault not found: {fixtures}", file=sys.stderr)
        return 2
    if shutil.which("basic-memory") is None:
        print("basic-memory CLI not found on PATH", file=sys.stderr)
        return 2

    embeddings = not args.no_embeddings and args.model_cache.is_dir()
    if not args.no_embeddings and not embeddings:
        log(f"model cache missing at {args.model_cache}; vector/hybrid cases skipped")

    if args.work_dir:
        work = args.work_dir.resolve()
        if work.exists():
            shutil.rmtree(work)
        work.mkdir(parents=True)
    else:
        work = Path(tempfile.mkdtemp(prefix="basic-memory-oracle-"))

    out = args.out.resolve()
    vault = work / "vault"
    config_dir = work / "config"
    home = work / "home"
    config_dir.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)

    shutil.copytree(fixtures, vault)
    model_cache_dst = work / "fastembed_cache"
    if embeddings:
        log(f"copying model cache {args.model_cache} -> {model_cache_dst}")
        shutil.copytree(args.model_cache, model_cache_dst)

    config = {
        "env": "user",
        "projects": {
            PROJECT: {
                "path": str(vault),
                "mode": "local",
                "workspace_id": None,
                "local_sync_path": None,
                "bisync_initialized": False,
                "last_sync": None,
            }
        },
        "default_project": PROJECT,
        "database_backend": "sqlite",
        "semantic_search_enabled": embeddings,
        # Un-flagged search cases must exercise pure FTS5; vector/hybrid cases pass
        # their explicit flags. Without this, the reference defaults to hybrid.
        "default_search_type": "text",
        "auto_update": False,
        "logfire_enabled": False,
        "logfire_send_to_logfire": False,
        "cli_output_style": "plain",
    }
    if embeddings:
        config["semantic_embedding_cache_dir"] = str(model_cache_dst)
    write_json(config_dir / "config.json", config)

    env = os.environ.copy()
    env["BASIC_MEMORY_CONFIG_DIR"] = str(config_dir)
    env["HOME"] = str(home)
    env["NO_COLOR"] = "1"
    env["TERM"] = "dumb"
    env.pop("BASIC_MEMORY_PROJECT", None)

    manifest: dict = {"project": PROJECT, "fixtures": str(fixtures.relative_to(REPO_ROOT)),
                      "embeddings": embeddings, "cases": []}

    log("reindex --full --search")
    reindex = run(["reindex", "--full", "--search", "--project", PROJECT], env, timeout=900)
    manifest["cases"].append({"id": "reindex-search", **{k: reindex[k] for k in ("exit_code", "seconds")}})
    if reindex["exit_code"] != 0:
        print(reindex["stdout"][-4000:], file=sys.stderr)
        print(reindex["stderr"][-4000:], file=sys.stderr)
        return 1

    if embeddings:
        log("reindex --embeddings")
        emb = run(["reindex", "--embeddings", "--project", PROJECT], env, timeout=1800)
        manifest["cases"].append({"id": "reindex-embeddings", **{k: emb[k] for k in ("exit_code", "seconds")}})
        if emb["exit_code"] != 0:
            print(emb["stdout"][-4000:], file=sys.stderr)
            print(emb["stderr"][-4000:], file=sys.stderr)
            return 1

    db_path = config_dir / "memory.db"
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    row = conn.execute("SELECT id FROM project WHERE name = ?", (PROJECT,)).fetchone()
    conn.close()
    if row is None:
        print("oracle project row missing after reindex", file=sys.stderr)
        return 1
    project_id = int(row[0])

    log("dumping parse/index rows")
    dump = dump_database(db_path, project_id)
    write_json(out / "parse" / "entities.json", canon_json(dump["entities"], work, out))
    write_json(out / "parse" / "observations.json", canon_json(dump["observations"], work, out))
    write_json(out / "parse" / "relations.json", canon_json(dump["relations"], work, out))
    write_json(out / "index" / "search-index.json", canon_json(dump["search_index"], work, out))

    log("capturing search cases")
    cases = list(SEARCH_CASES) + (VECTOR_CASES if embeddings else [])
    for case in cases:
        argv = ["tool", "search-notes"]
        if case["query"]:
            argv.append(case["query"])
        argv += case.get("extra", [])
        argv += ["--json", "--project", PROJECT, "--local"]
        result = run(argv, env, timeout=300)
        payload = parse_json_output(result)
        record = {
            "id": case["id"],
            "argv": argv,
            "exit_code": result["exit_code"],
            "seconds": result["seconds"],
            "parsed": payload is not None,
        }
        manifest["cases"].append(record)
        if payload is None:
            write_json(out / "search" / f"{case['id']}.json",
                       canon_json({"exit_code": result["exit_code"],
                                   "stdout": result["stdout"], "stderr": result["stderr"]}, work, out))
        else:
            write_json(out / "search" / f"{case['id']}.json", canon_json(payload, work, out))

    log("capturing context cases")
    raw_payloads = work / "raw-context"
    raw_payloads.mkdir(parents=True, exist_ok=True)
    for case in CONTEXT_CASES:
        argv = ["tool", "build-context", case["url"], *case.get("extra", []),
                "--json", "--project", PROJECT, "--local"]
        result = run(argv, env, timeout=300)
        payload = parse_json_output(result)
        manifest["cases"].append({
            "id": case["id"], "argv": argv, "exit_code": result["exit_code"],
            "seconds": result["seconds"], "parsed": payload is not None,
        })
        if payload is None:
            write_json(out / "context" / f"{case['id']}.json",
                       canon_json({"exit_code": result["exit_code"],
                                   "stdout": result["stdout"], "stderr": result["stderr"]}, work, out))
        else:
            write_json(out / "context" / f"{case['id']}.json", canon_json(payload, work, out))
            # `output_format="text"` is not reachable from the CLI (it always asks for
            # JSON), so replay the reference MCP formatter over the raw payload.
            raw = raw_payloads / f"{case['id']}.json"
            raw.write_text(json.dumps(payload, ensure_ascii=False))
            rendered = run_program(
                [
                    ORACLE_PYTHON,
                    str(REPO_ROOT / "tools" / "dump_reference_context_text.py"),
                    "--payload",
                    str(raw),
                    "--project",
                    PROJECT,
                ],
                env,
                timeout=300,
            )
            manifest["cases"].append({
                "id": f"{case['id']}-markdown", "argv": rendered["argv"],
                "exit_code": rendered["exit_code"], "seconds": rendered["seconds"],
            })
            if rendered["exit_code"] == 0:
                (out / "context" / f"{case['id']}.md").write_text(
                    canon_text(rendered["stdout"], work, out).rstrip("\n") + "\n"
                )
            else:
                print(rendered["stderr"][-2000:], file=sys.stderr)

    for case in CONTEXT_TEXT_CASES:
        argv = ["tool", "build-context", case["url"], *case.get("extra", []),
                "--plain", "--project", PROJECT, "--local"]
        result = run(argv, env, timeout=300)
        manifest["cases"].append({
            "id": f"{case['id']}-text", "argv": argv, "exit_code": result["exit_code"],
            "seconds": result["seconds"],
        })
        path = out / "context" / f"{case['id']}.txt"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(canon_text(result["stdout"], work, out) + "\n")

    log("capturing cli + error cases")
    status = run(["status", "--json", "--project", PROJECT], env, timeout=300)
    manifest["cases"].append({"id": "cli-status", "argv": status["argv"],
                              "exit_code": status["exit_code"], "seconds": status["seconds"],
                              "parsed": parse_json_output(status) is not None})
    write_json(out / "cli" / "status.json",
               canon_json(parse_json_output(status) or {"stdout": status["stdout"], "stderr": status["stderr"]},
                          work, out))

    for case in ERROR_CASES:
        argv = [*case["argv"], "--project", PROJECT, "--local"]
        result = run(argv, env, timeout=300)
        manifest["cases"].append({"id": case["id"], "argv": argv,
                                  "exit_code": result["exit_code"], "seconds": result["seconds"]})
        write_json(out / "errors" / f"{case['id']}.json", canon_json({
            "exit_code": result["exit_code"],
            "stdout": result["stdout"],
            "stderr": result["stderr"],
        }, work, out))

    log("dumping reference graph rows + find_related traversal")
    dump_graph = run_program(
        [
            ORACLE_PYTHON,
            str(REPO_ROOT / "tools" / "dump_reference_graph.py"),
            "--db",
            str(db_path),
        ],
        env,
        timeout=300,
    )
    manifest["cases"].append(
        {
            "id": "reference-graph-dump",
            "argv": dump_graph["argv"],
            "exit_code": dump_graph["exit_code"],
            "seconds": dump_graph["seconds"],
        }
    )
    if dump_graph["exit_code"] != 0:
        print(dump_graph["stdout"][-4000:], file=sys.stderr)
        print(dump_graph["stderr"][-4000:], file=sys.stderr)
        return 1

    if embeddings:
        # Reference vectors for the chunk corpus and the vector queries. Runs fully
        # offline against the copied model cache; the Rust side replays them through
        # `FixtureEmbeddingProvider` and checks its own ONNX runtime against them.
        log("dumping reference embeddings")
        reference_embeddings = run_program(
            [
                ORACLE_PYTHON,
                str(REPO_ROOT / "tools" / "dump_reference_embeddings.py"),
                "--model-cache",
                str(model_cache_dst),
            ],
            env,
            timeout=1800,
        )
        manifest["cases"].append(
            {
                "id": "reference-embeddings",
                "argv": reference_embeddings["argv"],
                "exit_code": reference_embeddings["exit_code"],
                "seconds": reference_embeddings["seconds"],
            }
        )
        if reference_embeddings["exit_code"] != 0:
            print(reference_embeddings["stdout"][-4000:], file=sys.stderr)
            print(reference_embeddings["stderr"][-4000:], file=sys.stderr)
            return 1

    normalized_vault = out / "vault"
    if normalized_vault.exists():
        shutil.rmtree(normalized_vault)
    shutil.copytree(vault, normalized_vault, ignore=shutil.ignore_patterns(".basic-memory", ".git"))
    normalized_hashes = {}
    for path in sorted(normalized_vault.rglob("*")):
        if path.is_file():
            rel = str(path.relative_to(normalized_vault))
            normalized_hashes[rel] = hashlib.sha256(path.read_bytes()).hexdigest()

    fixture_hashes = {}
    for path in sorted(fixtures.rglob("*")):
        if path.is_file():
            rel = str(path.relative_to(fixtures))
            fixture_hashes[rel] = hashlib.sha256(path.read_bytes()).hexdigest()

    version = subprocess.run(["basic-memory", "--version"], env=env, capture_output=True, text=True).stdout.strip()
    reference_env = {
        "reference": "basic-memory",
        "version_output": version,
        "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "oracle_python": sys.version.split()[0],
        "sqlite_version": sqlite3.sqlite_version,
        "embeddings": embeddings,
        "embedding_model": "BAAI/bge-small-en-v1.5" if embeddings else None,
        "config": {k: v for k, v in config.items() if k != "projects"},
        "config_notes": (
            "Only isolation/render/semantic knobs are overridden; algorithm-relevant "
            "settings (ensure_frontmatter_on_sync, index_changes, permalinks_include_project, "
            "update_permalinks_on_move, write_note_overwrite_default) keep reference defaults."
        ),
        "canonicalization": {
            "uuid": "regex-replaced with <uuid>",
            "timestamps": "ISO-8601 replaced with <timestamp>",
            "numeric_ids": sorted(sorted(ID_KEYS)),
            "paths": "workdir/repo paths replaced with <work>/<repo>",
        },
        "fixture_sha256": fixture_hashes,
        "normalized_vault_sha256": normalized_hashes,
    }
    write_json(out / "reference-env.json", reference_env)
    write_json(out / "manifest.json", manifest)

    log(f"golden written to {out}")
    if args.work_dir is None and not args.keep_workdir:
        shutil.rmtree(work, ignore_errors=True)
    else:
        log(f"workdir kept at {work}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
