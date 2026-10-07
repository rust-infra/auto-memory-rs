#!/usr/bin/env python3
"""Capture real MCP responses from the reference `basic-memory mcp` server.

Phase 11 needs the reference's *actual* `tools/call` payloads, not hand-written
expectations: `specs/mcp-spec.md` marks them as capture items `[G]`. This harness
sets up the same hermetic oracle environment as `export_reference.py` (temp HOME
and `BASIC_MEMORY_CONFIG_DIR`, never the real `~/.config/basic-memory`), indexes
the fixture vault, then drives the reference server over stdio and records every
response frame.

Run it with the sandbox escalated: the reference CLI's Alembic migration hangs
inside the Codex filesystem sandbox.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import export_reference as oracle  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT = REPO_ROOT / "tests" / "golden" / "mcp"
DEFAULT_FIXTURES = REPO_ROOT / "tests" / "fixtures" / "vault"

# One scripted session. Every frame is captured verbatim (after canonicalization),
# so the Rust tests can compare shapes and text instead of re-deriving them.
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
    {"id": "tools-list", "request": {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}},
    {
        "id": "list-directory-root",
        "request": {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "list_directory", "arguments": {}},
        },
    },
    {
        "id": "list-directory-notes-depth2",
        "request": {
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "list_directory",
                "arguments": {"dir_name": "/notes", "depth": 2, "page_size": 4},
            },
        },
    },
    {
        "id": "list-directory-notes-page2-sorted",
        "request": {
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "list_directory",
                "arguments": {
                    "dir_name": "/notes",
                    "depth": 2,
                    "sort": "updated_desc",
                    "page": 2,
                    "page_size": 4,
                },
            },
        },
    },
    {
        "id": "list-directory-glob",
        "request": {
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "list_directory",
                "arguments": {"dir_name": "/notes", "depth": 1, "file_name_glob": "*.md"},
            },
        },
    },
    {
        "id": "list-directory-json",
        "request": {
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "list_directory",
                "arguments": {"dir_name": "/notes", "depth": 1, "output_format": "json"},
            },
        },
    },
    {
        "id": "list-directory-page-beyond",
        "request": {
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "list_directory",
                "arguments": {"dir_name": "/notes", "depth": 1, "page": 9},
            },
        },
    },
    {
        "id": "read-content-markdown",
        "request": {
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {"name": "read_content", "arguments": {"path": "notes/simple.md"}},
        },
    },
    {
        "id": "read-content-permalink",
        "request": {
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {"name": "read_content", "arguments": {"path": "memory://notes/cjk"}},
        },
    },
    {
        "id": "view-note",
        "request": {
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "view_note",
                "arguments": {"identifier": "memory://notes/simple"},
            },
        },
    },
    {
        "id": "view-note-missing",
        "request": {
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "view_note",
                "arguments": {"identifier": "no-such-note-anywhere"},
            },
        },
    },
    {
        "id": "read-note-missing-text",
        "request": {
            "jsonrpc": "2.0",
            "id": 13,
            "method": "tools/call",
            "params": {
                "name": "read_note",
                "arguments": {"identifier": "no-such-note-anywhere"},
            },
        },
    },
    {
        "id": "recent-activity-json",
        "request": {
            "jsonrpc": "2.0",
            "id": 14,
            "method": "tools/call",
            "params": {
                "name": "recent_activity",
                "arguments": {"timeframe": "30d", "output_format": "json", "page_size": 5},
            },
        },
    },
    {
        "id": "list-memory-projects",
        "request": {
            "jsonrpc": "2.0",
            "id": 15,
            "method": "tools/call",
            "params": {"name": "list_memory_projects", "arguments": {}},
        },
    },
    {"id": "ping", "request": {"jsonrpc": "2.0", "id": 16, "method": "ping"}},
    {
        "id": "basic-memory-diagnostics",
        "request": {
            "jsonrpc": "2.0",
            "id": 26,
            "method": "tools/call",
            "params": {"name": "basic_memory_diagnostics", "arguments": {}},
        },
    },
    {
        "id": "recent-activity-text",
        "request": {
            "jsonrpc": "2.0",
            "id": 17,
            "method": "tools/call",
            "params": {
                "name": "recent_activity",
                "arguments": {"timeframe": "30d", "page_size": 5},
            },
        },
    },
    {
        "id": "recent-activity-empty-window",
        "request": {
            "jsonrpc": "2.0",
            "id": 18,
            "method": "tools/call",
            "params": {
                "name": "recent_activity",
                "arguments": {"timeframe": "1d", "page": 2, "page_size": 3},
            },
        },
    },
    {
        "id": "list-memory-projects-json",
        "request": {
            "jsonrpc": "2.0",
            "id": 19,
            "method": "tools/call",
            "params": {
                "name": "list_memory_projects",
                "arguments": {"output_format": "json"},
            },
        },
    },
    {
        "id": "read-note-text",
        "request": {
            "jsonrpc": "2.0",
            "id": 20,
            "method": "tools/call",
            "params": {
                "name": "read_note",
                "arguments": {"identifier": "memory://notes/simple"},
            },
        },
    },
    {
        "id": "read-note-json",
        "request": {
            "jsonrpc": "2.0",
            "id": 21,
            "method": "tools/call",
            "params": {
                "name": "read_note",
                "arguments": {
                    "identifier": "memory://notes/simple",
                    "output_format": "json",
                },
            },
        },
    },
    {
        "id": "read-note-json-frontmatter",
        "request": {
            "jsonrpc": "2.0",
            "id": 22,
            "method": "tools/call",
            "params": {
                "name": "read_note",
                "arguments": {
                    "identifier": "memory://notes/frontmatter",
                    "output_format": "json",
                    "include_frontmatter": True,
                },
            },
        },
    },
    {
        "id": "create-memory-project-constrained",
        "request": {
            "jsonrpc": "2.0",
            "id": 23,
            "method": "tools/call",
            "params": {
                "name": "create_memory_project",
                "arguments": {
                    "project_name": "scratch",
                    "project_path": "/tmp/scratch-vault",
                },
            },
        },
    },
    {
        "id": "create-memory-project-constrained-json",
        "request": {
            "jsonrpc": "2.0",
            "id": 24,
            "method": "tools/call",
            "params": {
                "name": "create_memory_project",
                "arguments": {
                    "project_name": "scratch",
                    "project_path": "/tmp/scratch-vault",
                    "output_format": "json",
                },
            },
        },
    },
    {
        "id": "delete-project-constrained",
        "request": {
            "jsonrpc": "2.0",
            "id": 25,
            "method": "tools/call",
            "params": {
                "name": "delete_project",
                "arguments": {"project_name": "scratch"},
            },
        },
    },
]


def log(message: str) -> None:
    print(f"[mcp-oracle] {message}", flush=True)


def drive_server(binary: str, env: dict, session: list[dict], timeout: int = 300) -> dict:
    """Start `basic-memory mcp` and run the session frame by frame.

    The server must stay interactive: closing stdin after a single write makes
    FastMCP tear down the transport and answer every pending request with
    "Connection closed". So each request is written, flushed, and its matching
    response read before the next one goes out.
    """
    started = time.perf_counter()
    proc = subprocess.Popen(
        [binary, "mcp", "--project", oracle.PROJECT],
        env=env,
        cwd=str(REPO_ROOT),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    assert proc.stdout is not None and proc.stdin is not None and proc.stderr is not None

    frames: queue.Queue[str] = queue.Queue()
    diagnostics: list[str] = []

    def pump(stream, sink) -> None:
        for line in stream:
            sink(line)
        sink(None)

    threading.Thread(
        target=pump, args=(proc.stdout, frames.put), daemon=True
    ).start()
    threading.Thread(
        target=pump, args=(proc.stderr, diagnostics.append), daemon=True
    ).start()

    responses: dict[object, dict] = {}
    deadline = time.monotonic() + timeout

    def await_id(request_id: object) -> None:
        while time.monotonic() < deadline:
            try:
                line = frames.get(timeout=1.0)
            except queue.Empty:
                if proc.poll() is not None:
                    break
                continue
            if line is None:
                break
            if not line.strip():
                continue
            try:
                frame = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(frame, dict) and frame.get("id") == request_id:
                responses[request_id] = frame
                return
            if isinstance(frame, dict) and frame.get("method"):
                # Server-initiated notification; keep it out of the response set.
                diagnostics.append(line)

    for entry in session:
        request = entry["request"]
        proc.stdin.write(json.dumps(request, ensure_ascii=False) + "\n")
        proc.stdin.flush()
        await_id(request["id"])
        if entry["id"] == "initialize":
            # FastMCP only accepts tools/list after the initialized notification.
            proc.stdin.write(
                json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"
            )
            proc.stdin.flush()
        if proc.poll() is not None:
            break

    proc.stdin.close()
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()

    return {
        "seconds": round(time.perf_counter() - started, 3),
        "exit_code": proc.returncode,
        "responses": responses,
        "invalidated": proc.returncode != 0,
        "stderr": "".join(line for line in diagnostics if line),
    }


def collect_frames(responses: dict, session: list[dict], workdir: Path, out: Path) -> list[dict]:
    """Pair each parsed frame with the request that produced it."""
    captured = []
    for entry in session:
        request_id = entry["request"]["id"]
        frame = responses.get(request_id)
        if frame is not None and "error" in frame:
            # Protocol errors are still real reference behavior worth recording.
            frame = {**frame}
        captured.append(
            {
                "id": entry["id"],
                "request": entry["request"],
                "frame": oracle.canon_json(frame, workdir, out) if frame else None,
            }
        )
    return captured


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
        work = Path(tempfile.mkdtemp(prefix="basic-memory-mcp-oracle-"))

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
        out / "responses.json",
        {
            "reference": "basic-memory",
            "project": oracle.PROJECT,
            "protocolVersion": "2024-11-05",
            "session": [entry["id"] for entry in SESSION],
            "responses": captured,
        },
    )
    log(f"wrote {out / 'responses.json'} ({len(captured)} frames)")

    if args.keep_workdir:
        log(f"workdir kept at {work}")
    else:
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
