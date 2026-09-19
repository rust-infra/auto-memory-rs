#!/usr/bin/env python3
"""Capture the reference note-write surface: metadata merge, directory guard, and the
directory variants of `delete_note` / `move_note`.

`write_note` accepts `note_type` and `tags` in addition to `metadata`, and merges them into
the note's frontmatter with a specific precedence. It also validates the target directory
against the project boundary and answers with a structured refusal rather than an error.
`delete_note` and `move_note` both take `is_directory`, which switches them from a single
note to a whole subtree.

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
    # note_type + tags land in the frontmatter.
    call(2, "write_note", {"title": "Typed Note", "content": "# Typed Note\n\n- [role] writer\n",
                           "directory": "people", "note_type": "person",
                           "tags": ["author", "#ship-it"], "output_format": "json"}),
    call(3, "read_note", {"identifier": "people/typed-note", "output_format": "json",
                          "include_frontmatter": True}),
    # Explicit tags win over `metadata["tags"]`; other metadata keys survive.
    call(4, "write_note", {"title": "Merged Note", "content": "# Merged Note\n",
                           "directory": "notes", "note_type": "meeting",
                           "tags": "alpha,beta",
                           "metadata": {"tags": ["ignored"], "status": "active"},
                           "output_format": "json"}),
    call(5, "read_note", {"identifier": "notes/merged-note", "output_format": "json",
                          "include_frontmatter": True}),
    # The content's own frontmatter `type` stays authoritative.
    call(6, "write_note", {"title": "Content Type", "content":
                           "---\ntype: reference\n---\n\n# Content Type\n",
                           "directory": "notes", "note_type": "person",
                           "output_format": "json"}),
    call(7, "read_note", {"identifier": "notes/content-type", "output_format": "json",
                          "include_frontmatter": True}),
    # The directory guard: a traversal and an absolute path are refused, `/` is the root.
    call(8, "write_note", {"title": "Escape", "content": "# Escape",
                           "directory": "../outside"}),
    call(9, "write_note", {"title": "Escape", "content": "# Escape",
                           "directory": "../outside", "output_format": "json"}),
    call(10, "write_note", {"title": "At Root", "content": "# At Root", "directory": "/",
                            "output_format": "json"}),
    # Directory deletion and moving.
    call(11, "delete_note", {"identifier": "projects", "is_directory": True}),
    call(12, "delete_note", {"identifier": "notes/empty", "output_format": "json"}),
    call(13, "move_note", {"identifier": "notes/nested", "destination_folder": "archive",
                           "is_directory": True, "output_format": "json"}),
    call(14, "read_note", {"identifier": "archive/deep-note", "output_format": "json"}),
    # Missing identifiers and the successful move variants.
    call(15, "delete_note", {"identifier": "no-such-note-anywhere"}),
    call(16, "delete_note", {"identifier": "no-such-note-anywhere", "output_format": "json"}),
    call(17, "move_note", {"identifier": "notes/simple", "destination_path":
                           "archive/simple-moved.md", "output_format": "json"}),
    call(18, "move_note", {"identifier": "notes/nested", "destination_path": "archive/nested",
                           "is_directory": True, "output_format": "json"}),
    call(19, "read_note", {"identifier": "archive/nested/deep-note", "output_format": "json"}),
    call(20, "move_note", {"identifier": "notes/relations", "destination_folder": "archive",
                           "output_format": "json"}),
    call(21, "read_note", {"identifier": "archive/relations", "output_format": "json"}),
    # Writing the same note again: the reference creates optimistically and falls back to
    # an update on conflict.
    call(22, "write_note", {"title": "At Root", "content": "# At Root\n\n- [fact] again",
                            "directory": "/", "output_format": "json"}),
    # Overwrite, a directory deleted through the JSON surface, and two failure codes.
    call(23, "write_note", {"title": "At Root", "content": "# At Root\n\n- [fact] replaced",
                            "directory": "/", "overwrite": True, "output_format": "json"}),
    call(24, "delete_note", {"identifier": "duplicates", "is_directory": True,
                             "output_format": "json"}),
    call(25, "delete_note", {"identifier": "no-such-directory", "is_directory": True,
                             "output_format": "json"}),
    call(26, "move_note", {"identifier": "no-such-note-anywhere",
                           "destination_path": "archive/nope.md", "output_format": "json"}),
    call(27, "move_note", {"identifier": "notes/cjk", "destination_path": "archive/cjk.md",
                           "output_format": "json"}),
    call(28, "move_note", {"identifier": "notes/cjk", "destination_path": "archive/cjk.md",
                           "output_format": "json"}),
    # The default text surfaces for a successful write and move.
    call(29, "write_note", {"title": "Text Surface", "content": "# Text Surface",
                            "directory": "notes"}),
    call(30, "move_note", {"identifier": "notes/text-surface",
                           "destination_path": "archive/text-surface.md"}),
    call(31, "delete_note", {"identifier": "archive/text-surface.md"}),
    # edit_note surfaces: JSON, the default text summary, and a miss.
    call(32, "write_note", {"title": "Editable", "content": "# Editable\n\n- [fact] first\n",
                            "directory": "notes", "output_format": "json"}),
    call(33, "edit_note", {"identifier": "notes/editable", "operation": "append",
                           "content": "- [fact] appended", "output_format": "json"}),
    call(34, "edit_note", {"identifier": "notes/editable", "operation": "append",
                           "content": "- [fact] text mode"}),
    call(35, "edit_note", {"identifier": "notes/no-such-editable", "operation": "append",
                           "content": "- [fact] nowhere", "output_format": "json"}),
    call(36, "edit_note", {"identifier": "notes/no-such-editable", "operation": "append",
                           "content": "- [fact] nowhere"}),
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
        work = Path(tempfile.mkdtemp(prefix="basic-memory-note-oracle-"))

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

    # The filesystem side of the same operations, so the byte-level result is recorded too.
    files = {}
    for relative in [
        "people/typed-note.md",
        "notes/merged-note.md",
        "notes/content-type.md",
        "At Root.md",
        "archive/deep-note.md",
        "archive/nested/deep-note.md",
        "archive/simple-moved.md",
        "archive/relations.md",
        "archive/cjk.md",
        "duplicates/dup-a/same-title.md",
        "notes/nested/deep-note.md",
        "notes/simple.md",
        "notes/relations.md",
        "notes/cjk.md",
        "notes/Text Surface.md",
        "archive/text-surface.md",
        "notes/Editable.md",
        "notes/no-such-editable.md",
        "projects/alpha.md",
    ]:
        path = vault / relative
        files[relative] = path.read_text(encoding="utf-8") if path.is_file() else None

    oracle.write_json(
        out / "note-tools.json",
        {
            "reference": "basic-memory",
            "project": oracle.PROJECT,
            "protocolVersion": "2024-11-05",
            "responses": captured,
            "files": files,
        },
    )
    log(f"wrote {out / 'note-tools.json'} ({len(captured)} frames)")

    if args.keep_workdir:
        log(f"workdir kept at {work}")
    else:
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
