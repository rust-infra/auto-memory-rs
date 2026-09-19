#!/usr/bin/env python3
"""Capture the reference `search_notes` tool across every `search_type`.

`search_notes` maps `search_type` onto the query it builds (text / title / permalink /
vector / semantic / hybrid), and the non-semantic modes need no model at all. The
semantic modes do: with `semantic_search_enabled=false` the reference answers with
guidance rather than results, and an unknown type raises. Those branches are the reason
this harness exists — this port was silently serving a *text* search for
`search_type="vector"`, which is a wrong answer rather than a missing feature.

Run it with the sandbox escalated: the reference CLI's Alembic migration hangs inside
the Codex filesystem sandbox.
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
from dump_reference_mcp import collect_frames, drive_server, log  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT = REPO_ROOT / "tests" / "golden" / "mcp"
DEFAULT_FIXTURES = REPO_ROOT / "tests" / "fixtures" / "vault"


def call(request_id: int, arguments: dict) -> dict:
    label = arguments.get("search_type", "default")
    return {
        "id": f"{request_id}-search-notes-{label}",
        "request": {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": "search_notes", "arguments": arguments},
        },
    }


# Semantic search stays disabled (the harness config), so the vector-shaped requests are
# exactly the ones that must not silently become text searches.
SESSION: list[dict] = [
    {
        "id": "initialize",
        "request": {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "oracle-harness", "version": "0"},
            },
        },
    },
    call(2, {"query": "rust", "search_type": "text", "output_format": "json"}),
    call(3, {"query": "Alpha", "search_type": "title", "output_format": "json"}),
    call(4, {"query": "projects/*", "search_type": "permalink", "output_format": "json"}),
    call(5, {"query": "projects/alpha", "search_type": "permalink", "output_format": "json"}),
    call(6, {"query": "rust", "search_type": "vector"}),
    call(7, {"query": "rust", "search_type": "hybrid"}),
    call(8, {"query": "rust", "search_type": "semantic"}),
    call(9, {"query": "rust", "search_type": "nonsense"}),
    call(10, {"query": "rust", "search_type": "title"}),
    # A constrained server has no project list to fan out to, so the all-projects path
    # answers with an empty page instead of searching.
    call(11, {"query": "rust", "search_all_projects": True, "output_format": "json"}),
    call(12, {"query": "rust", "search_all_projects": True}),
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--work-dir", type=Path, default=None)
    parser.add_argument("--keep-workdir", action="store_true")
    args = parser.parse_args()

    fixtures = args.fixtures.resolve()
    if not fixtures.is_dir():
        print(f"fixture vault not found: {fixtures}", file=sys.stderr)
        return 2
    binary = shutil.which("basic-memory")
    if binary is None:
        print("basic-memory CLI not found on PATH", file=sys.stderr)
        return 2

    if args.work_dir:
        work = args.work_dir.resolve()
        if work.exists():
            shutil.rmtree(work)
        work.mkdir(parents=True)
    else:
        work = Path(tempfile.mkdtemp(prefix="basic-memory-search-oracle-"))

    out = args.out.resolve()
    vault = work / "vault"
    config_dir = work / "config"
    home = work / "home"
    config_dir.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    shutil.copytree(fixtures, vault)

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
            # Semantic search stays off: the vector-shaped requests must say so instead
            # of quietly running a text search.
            "semantic_search_enabled": False,
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

    log("reindex --full --search")
    reindex = oracle.run(
        ["reindex", "--full", "--search", "--project", oracle.PROJECT], env, timeout=900
    )
    if reindex["exit_code"] != 0:
        print(reindex["stdout"][-4000:], file=sys.stderr)
        print(reindex["stderr"][-4000:], file=sys.stderr)
        return 1

    log("mcp session")
    session = drive_server(binary, env, SESSION)
    captured = collect_frames(session["responses"], SESSION, work, out)
    missing = [entry["id"] for entry in captured if entry["frame"] is None]
    if missing:
        print(f"no response for: {missing}", file=sys.stderr)
        print(session["stderr"][-4000:], file=sys.stderr)
        return 1

    oracle.write_json(
        out / "search-types.json",
        {
            "reference": "basic-memory",
            "project": oracle.PROJECT,
            "protocolVersion": "2024-11-05",
            "semantic_search_enabled": False,
            "responses": captured,
        },
    )
    log(f"wrote {out / 'search-types.json'} ({len(captured)} frames)")

    if args.keep_workdir:
        log(f"workdir kept at {work}")
    else:
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
