#!/usr/bin/env python3
"""Capture the reference `search`/`fetch` ChatGPT adapters.

`fetch` and `search` are the two MCP tools Basic Memory exposes for OpenAI's MCP
clients. They are gated by `is_openai_mcp_client`, which reads the `clientInfo` of the
`initialize` request (`name`/`title`, case-insensitive, exactly `openai-mcp` or prefixed
`openai-mcp/`). A non-OpenAI client does not get an error — it gets a well-formed
payload explaining that the tool is OpenAI-only, so this harness records both sides:

* the same session driven by a neutral client (the refusal payloads);
* a session driven by `openai-mcp` (the real search/fetch documents, including a
  missing document and identifier normalization).

Both surfaces matter: `search`/`fetch` return a *list* of MCP content items rather than
a `str` or `dict`, so their result frame differs from every other tool in the server.

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


def initialize(request_id: int, client_name: str) -> dict:
    return {
        "id": "initialize",
        "request": {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": client_name, "version": "0"},
            },
        },
    }


def call(request_id: int, name: str, arguments: dict) -> dict:
    return {
        "id": f"{request_id}-{name}",
        "request": {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        },
    }


# A neutral client: both adapters refuse.
NEUTRAL_SESSION: list[dict] = [
    initialize(1, "oracle-harness"),
    call(2, "search", {"query": "rust"}),
    call(3, "fetch", {"id": "memory://notes/simple"}),
]

# `openai-mcp`: the adapters run for real.
OPENAI_SESSION: list[dict] = [
    initialize(1, "openai-mcp"),
    call(2, "search", {"query": "rust"}),
    call(3, "search", {"query": "zzz-nothing-matches-this"}),
    call(4, "fetch", {"id": "oracle/notes/simple"}),
    call(5, "fetch", {"id": "notes/simple.md"}),
    call(6, "fetch", {"id": "memory://notes/cjk"}),
    call(7, "fetch", {"id": "Wikilinks Demo"}),
    call(8, "fetch", {"id": "no-such-note-anywhere"}),
]

SUITES: list[dict] = [
    {"name": "neutral-client", "session": NEUTRAL_SESSION},
    {"name": "openai-mcp-client", "session": OPENAI_SESSION},
]


def run_suite(binary: str, suite: dict, fixtures: Path, out: Path, work_root: Path) -> dict:
    work = work_root / suite["name"]
    vault = work / "vault"
    config_dir = work / "config"
    home = work / "home"
    config_dir.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    # `shutil.copytree` copies with `copy2`, so the fixture mtimes survive — several
    # captured payloads are ordered by `updated_at`.
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

    log(f"[{suite['name']}] reindex --full --search")
    reindex = oracle.run(
        ["reindex", "--full", "--search", "--project", oracle.PROJECT], env, timeout=900
    )
    if reindex["exit_code"] != 0:
        print(reindex["stdout"][-4000:], file=sys.stderr)
        print(reindex["stderr"][-4000:], file=sys.stderr)
        raise SystemExit(1)

    log(f"[{suite['name']}] mcp session")
    session = drive_server(binary, env, suite["session"])
    captured = collect_frames(session["responses"], suite["session"], work, out)
    missing = [entry["id"] for entry in captured if entry["frame"] is None]
    if missing:
        print(f"[{suite['name']}] no response for: {missing}", file=sys.stderr)
        print(session["stderr"][-4000:], file=sys.stderr)
        raise SystemExit(1)
    return {"name": suite["name"], "responses": captured}


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
        work_root = args.work_dir.resolve()
        if work_root.exists():
            shutil.rmtree(work_root)
        work_root.mkdir(parents=True)
    else:
        work_root = Path(tempfile.mkdtemp(prefix="basic-memory-chatgpt-oracle-"))

    out = args.out.resolve()
    suites = [run_suite(binary, suite, fixtures, out, work_root) for suite in SUITES]

    oracle.write_json(
        out / "chatgpt.json",
        {
            "reference": "basic-memory",
            "project": oracle.PROJECT,
            "protocolVersion": "2024-11-05",
            "suites": suites,
        },
    )
    total = sum(len(suite["responses"]) for suite in suites)
    log(f"wrote {out / 'chatgpt.json'} ({total} frames, {len(suites)} suites)")

    if args.keep_workdir:
        log(f"workdir kept at {work_root}")
    else:
        shutil.rmtree(work_root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
