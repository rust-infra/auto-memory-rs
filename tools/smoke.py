#!/usr/bin/env python3
"""Offline end-to-end smoke check.

Builds the release binary with no network, then exercises the whole local surface against
a throwaway vault: index, status, search, context, schema, the MCP server over stdio, and
recovery after deleting the index. The vault is hashed before and after to prove that
indexing never rewrites the user's files.

Usage:

    python3 tools/smoke.py [--skip-build]

Exits non-zero on the first failed expectation, so it works as a release gate:

    python3 tools/smoke.py && echo releasable
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
RELEASE_BIN = REPO_ROOT / "target" / "release" / "basic-mem"
FIXTURE_VAULT = REPO_ROOT / "tests" / "fixtures" / "vault"


def step(title: str) -> None:
    print(f"\n== {title}", flush=True)


def fail(message: str) -> None:
    print(f"SMOKE FAILED: {message}", file=sys.stderr)
    raise SystemExit(1)


def run(argv: list[str]) -> str:
    result = subprocess.run(argv, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        fail(f"{' '.join(argv)} exited {result.returncode}\n{result.stderr[-2000:]}")
    return result.stdout


def vault_hash(vault: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(vault.rglob("*")):
        if path.is_file():
            digest.update(str(path.relative_to(vault)).encode())
            digest.update(path.read_bytes())
    return digest.hexdigest()


def drive_mcp(binary: Path, vault: Path, index: Path) -> tuple[set[str], dict]:
    """One scripted MCP session: initialize, tools/list, read_note."""
    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}},
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "read_note",
                "arguments": {"identifier": "memory://notes/simple", "output_format": "json"},
            },
        },
    ]
    session = subprocess.run(
        [str(binary), "mcp", "--vault", str(vault), "--index", str(index), "--project", "smoke"],
        input="\n".join(json.dumps(request) for request in requests) + "\n",
        capture_output=True,
        text=True,
        check=False,
    )
    if session.returncode != 0:
        fail(f"mcp server exited {session.returncode}\n{session.stderr[-2000:]}")
    if "jsonrpc" in session.stderr:
        fail("protocol frames must not leak to stderr")

    frames = [json.loads(line) for line in session.stdout.splitlines() if line.strip()]
    if len(frames) != 3:
        fail(f"expected 3 frames, got {len(frames)}")
    names = {tool["name"] for tool in frames[1]["result"]["tools"]}
    note = json.loads(frames[2]["result"]["content"][0]["text"])
    return names, note


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true", help="reuse an existing release build")
    args = parser.parse_args()

    if not args.skip_build:
        step("build (offline, release)")
        run(["cargo", "build", "--offline", "--release"])
    if not RELEASE_BIN.is_file():
        fail(f"missing {RELEASE_BIN}")

    work = Path(tempfile.mkdtemp(prefix="basic-mem-rs-smoke-"))
    try:
        vault = work / "vault"
        index = work / "memory.db"
        shutil.copytree(FIXTURE_VAULT, vault)
        binary = RELEASE_BIN
        # Flags shared by every command; the binary itself is always argv[0].
        base = ["--index", str(index), "--project", "smoke"]

        step("reindex --full")
        report = json.loads(
            run([str(binary), "reindex", "--vault", str(vault), *base, "--full"])
        )
        if report["documents_indexed"] <= 0:
            fail(f"nothing indexed: {report}")
        print(
            f"  indexed {report['documents_indexed']} notes "
            f"(skipped {report['documents_skipped']}), "
            f"{report['relations_resolved']} relations resolved"
        )

        step("status")
        counts = json.loads(run([str(binary), "status", *base]))
        if counts["entities"] <= 0 or counts["observations"] <= 0:
            fail(f"empty index: {counts}")
        print(
            f"  entities={counts['entities']} observations={counts['observations']} "
            f"relations={counts['relations']}"
        )

        step("search")
        payload = json.loads(run([str(binary), "search", *base, "rust"]))
        results = payload["results"]
        if not results:
            fail("no result for 'rust'")
        print(f"  {len(results)} hits, top={results[0]['permalink']}")

        step("context")
        outline = run([str(binary), "context", "memory://notes/simple", *base, "--plain"])
        if "# Simple Note" not in outline:
            fail(f"context output looks wrong:\n{outline[:400]}")
        print("  " + outline.strip().splitlines()[0])

        step("schema")
        guidance = run([str(binary), "schema", "validate", *base, "--text"])
        if "No Schemas Defined" not in guidance:
            fail(f"unexpected schema guidance:\n{guidance[:400]}")
        print("  " + guidance.strip().splitlines()[0])

        step("mcp session over stdio")
        names, note = drive_mcp(binary, vault, index)
        for expected in ("write_note", "read_note", "search_notes", "build_context",
                         "schema_validate", "search", "fetch"):
            if expected not in names:
                fail(f"missing tool {expected}")
        if note["file_path"] != "notes/simple.md":
            fail(f"read_note returned {note}")
        print(f"  {len(names)} tools, read_note returned {note['file_path']}")

        step("recovery after deleting the index")
        before = vault_hash(vault)
        index.unlink()
        run([str(binary), "reindex", "--vault", str(vault), *base, "--full"])
        if vault_hash(vault) != before:
            fail("indexing rewrote vault files")
        rebuilt = json.loads(run([str(binary), "status", *base]))
        if rebuilt["entities"] != counts["entities"]:
            fail(f"rebuild produced {rebuilt}, expected {counts}")
        print(f"  rebuilt {rebuilt['entities']} entities; the vault is byte-identical")
    finally:
        shutil.rmtree(work, ignore_errors=True)

    print("\nSMOKE OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
