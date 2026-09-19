#!/usr/bin/env python3
"""Capture the reference `schema_*` MCP responses.

Phase 12–13's tool surface is the three schema tools: `schema_validate`,
`schema_infer`, and `schema_diff`. They are user-visible through MCP (text guidance vs
report markdown) and through their JSON payloads, and both surfaces have branches that
only a real server can settle:

* which notes a request covers (`note_type`, identifier, or all schema-covered types);
* the guard order that decides "no schemas" / "no notes" / "no schema" guidance;
* the exact report payloads, including dropped `null`s.

This harness sets up the same hermetic oracle environment as `export_reference.py`
(temp HOME and `BASIC_MEMORY_CONFIG_DIR`, never the real `~/.config/basic-memory`),
indexes each suite's vault, drives the reference server over stdio, and records every
response frame verbatim (canonicalized).

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
DEFAULT_FIXTURES = REPO_ROOT / "tests" / "fixtures"


def call(request_id: int, name: str, arguments: dict) -> dict:
    return {
        "id": f"{request_id}-{name}-{'-'.join(sorted(arguments)) or 'default'}",
        "request": {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        },
    }


# `schema_validate`: every coverage mode and both guard branches.
SCHEMA_SESSION: list[dict] = [
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
    call(2, "schema_validate", {"note_type": "person"}),
    call(3, "schema_validate", {"note_type": "person", "output_format": "json"}),
    call(4, "schema_validate", {"note_type": "Person"}),
    call(5, "schema_validate", {"identifier": "people/ada-lovelace"}),
    call(6, "schema_validate", {"identifier": "people/ada-lovelace", "output_format": "json"}),
    call(7, "schema_validate", {}),
    call(8, "schema_validate", {"output_format": "json"}),
    call(9, "schema_validate", {"note_type": "ghost"}),
    call(10, "schema_validate", {"note_type": "ghost", "output_format": "json"}),
    call(11, "schema_validate", {"note_type": "meeting"}),
    call(12, "schema_validate", {"note_type": "meeting", "output_format": "json"}),
    call(13, "schema_validate", {"identifier": "no-such-note"}),
    call(14, "schema_validate", {"identifier": "no-such-note", "output_format": "json"}),
    call(15, "schema_validate", {"note_type": "project", "output_format": "json"}),
    call(16, "schema_infer", {"note_type": "person"}),
    call(17, "schema_infer", {"note_type": "person", "output_format": "json"}),
    call(18, "schema_infer", {"note_type": "person", "threshold": 0.5}),
    call(19, "schema_infer", {"note_type": "meeting"}),
    call(20, "schema_infer", {"note_type": "meeting", "output_format": "json"}),
    call(21, "schema_infer", {"note_type": "ghost", "output_format": "json"}),
    call(22, "schema_diff", {"note_type": "person"}),
    call(23, "schema_diff", {"note_type": "person", "output_format": "json"}),
    call(24, "schema_diff", {"note_type": "project", "output_format": "json"}),
    call(25, "schema_diff", {"note_type": "meeting"}),
    call(26, "schema_diff", {"note_type": "meeting", "output_format": "json"}),
]

# The all-types guard needs a project with notes but no schema notes at all.
NO_SCHEMA_SESSION: list[dict] = [
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
    call(2, "schema_validate", {}),
    call(3, "schema_validate", {"output_format": "json"}),
]

# A schema whose `settings.validation` is not a known mode: the parser raises, the API
# answers 400, and the tool renders its failure template.
BROKEN_SESSION: list[dict] = [
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
    call(2, "schema_validate", {"note_type": "person"}),
    call(3, "schema_validate", {"note_type": "person", "output_format": "json"}),
    call(4, "schema_diff", {"note_type": "person"}),
]

SUITES: list[dict] = [
    {
        "name": "schema-vault",
        "fixture": "schema-vault",
        "session": SCHEMA_SESSION,
        "overrides": {},
    },
    {
        "name": "no-schema-vault",
        "fixture": "vault",
        "session": NO_SCHEMA_SESSION,
        "overrides": {},
    },
    {
        "name": "broken-schema-vault",
        "fixture": "schema-vault",
        "session": BROKEN_SESSION,
        "overrides": {
            "schema/person.md": (
                "---\n"
                "title: Person\n"
                "type: schema\n"
                "entity: person\n"
                "version: 1\n"
                "schema:\n"
                "  name: string, full name\n"
                "settings:\n"
                "  validation: nonsense\n"
                "---\n"
                "\n"
                "# Person Schema\n"
            )
        },
    },
]

# `<harness> tool schema-*` is the reference's JSON CLI surface: it calls the MCP tool
# with `output_format="json"` and prints `json.dumps(result, indent=2, ensure_ascii=True)`.
CLI_COMMANDS: dict[str, list[list[str]]] = {
    "schema-vault": [
        ["tool", "schema-validate", "person"],
        ["tool", "schema-validate", "Person"],
        ["tool", "schema-validate", "people/ada-lovelace.md"],
        ["tool", "schema-validate"],
        ["tool", "schema-validate", "ghost"],
        ["tool", "schema-validate", "meeting"],
        ["tool", "schema-infer", "person"],
        ["tool", "schema-infer", "person", "--threshold", "0.5"],
        ["tool", "schema-infer", "meeting"],
        ["tool", "schema-diff", "person"],
        ["tool", "schema-diff", "meeting"],
    ],
    "no-schema-vault": [["tool", "schema-validate"]],
    "broken-schema-vault": [
        ["tool", "schema-validate", "person"],
        ["tool", "schema-diff", "person"],
    ],
}


def run_suite(binary: str, suite: dict, fixtures: Path, out: Path, work_root: Path) -> dict:
    """Index one fixture vault and replay one scripted session against it."""
    work = work_root / suite["name"]
    vault = work / "vault"
    config_dir = work / "config"
    home = work / "home"
    config_dir.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    shutil.copytree(fixtures / suite["fixture"], vault)
    for relative_path, content in suite["overrides"].items():
        target = vault / relative_path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")

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

    cli = []
    for argv in CLI_COMMANDS.get(suite["name"], []):
        log(f"[{suite['name']}] {' '.join(argv)}")
        result = oracle.run([*argv, "--project", oracle.PROJECT], env, timeout=300)
        cli.append(
            {
                "id": " ".join(argv),
                "argv": [*argv, "--project", oracle.PROJECT],
                "exit_code": result["exit_code"],
                "stdout": oracle.canon_text(result["stdout"], work, out),
            }
        )

    return {
        "name": suite["name"],
        "fixture": suite["fixture"],
        "overrides": sorted(suite["overrides"]),
        "responses": captured,
        "cli": cli,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--work-dir", type=Path, default=None)
    parser.add_argument("--keep-workdir", action="store_true")
    args = parser.parse_args()

    fixtures = args.fixtures.resolve()
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
        work_root = Path(tempfile.mkdtemp(prefix="basic-memory-schema-oracle-"))

    out = args.out.resolve()
    suites = [run_suite(binary, suite, fixtures, out, work_root) for suite in SUITES]

    oracle.write_json(
        out / "schema.json",
        {
            "reference": "basic-memory",
            "project": oracle.PROJECT,
            "protocolVersion": "2024-11-05",
            "suites": suites,
        },
    )
    total = sum(len(suite["responses"]) for suite in suites)
    log(f"wrote {out / 'schema.json'} ({total} frames, {len(suites)} suites)")

    if args.keep_workdir:
        log(f"workdir kept at {work_root}")
    else:
        shutil.rmtree(work_root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
