#!/usr/bin/env python3
"""Dump raw reference parser output for the fixture vault.

Run with the *reference* interpreter, e.g.:

    ~/.local/share/uv/tools/basic-memory/bin/python \
        tools/dump_reference_parse.py

This captures the parse layer (frontmatter + observations + relations) BEFORE the
indexer/sync layer rewrites files, which is what the Rust parser must match first.
"""

from __future__ import annotations

import argparse
import asyncio
import json
from pathlib import Path


async def dump(vault: Path) -> dict:
    from basic_memory.markdown.entity_parser import EntityParser

    parser = EntityParser(vault)
    out: dict = {}
    for path in sorted(vault.rglob("*.md")):
        rel = path.relative_to(vault).as_posix()
        if any(part.startswith(".") for part in Path(rel).parts):
            continue
        try:
            doc = await parser.parse_file(path)
        except Exception as exc:  # noqa: BLE001 - record reference failure verbatim
            out[rel] = {"error": type(exc).__name__, "message": str(exc)}
            continue
        out[rel] = {
            "title": doc.frontmatter.title,
            "type": doc.frontmatter.type,
            "permalink": doc.frontmatter.permalink,
            "tags": list(doc.frontmatter.tags),
            "metadata": doc.frontmatter.metadata,
            "content": doc.content,
            "observations": [obs.model_dump() for obs in doc.observations],
            "relations": [rel.model_dump() for rel in doc.relations],
        }
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    repo = Path(__file__).resolve().parents[1]
    parser.add_argument("--vault", type=Path, default=repo / "tests" / "fixtures" / "vault")
    parser.add_argument("--out", type=Path, default=repo / "tests" / "golden" / "parse" / "reference-parse.json")
    args = parser.parse_args()

    data = asyncio.run(dump(args.vault.resolve()))
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(data, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(f"wrote {args.out} ({len(data)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
