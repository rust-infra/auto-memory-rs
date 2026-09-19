#!/usr/bin/env python3
"""Render a captured `build_context` payload with the reference MCP formatter.

Run with the *reference* interpreter:

    /home/rg/.local/share/uv/tools/basic-memory/bin/python \
        tools/dump_reference_context_text.py --payload <raw-payload.json> --project oracle

`bm tool build-context --json` always asks the MCP tool for ``output_format="json"``
and renders text client-side (`_plain_build_context`), so the ``output_format="text"``
markdown formatter is only reachable by calling the tool module directly. This script
replays it over the payload the harness captured, which keeps the golden tied to
reference code rather than to a hand-written expectation.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--payload", type=Path, required=True, help="raw JSON payload")
    parser.add_argument("--project", default="oracle", help="active project name")
    parser.add_argument("--out", type=Path, default=None, help="optional output file")
    args = parser.parse_args()

    from basic_memory.mcp.tools.build_context import _format_context_markdown
    from basic_memory.schemas.memory import GraphContext

    payload = json.loads(args.payload.read_text())
    graph = GraphContext.model_validate(payload)
    text = _format_context_markdown(graph, args.project)
    if args.out is not None:
        args.out.write_text(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
