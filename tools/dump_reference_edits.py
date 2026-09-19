#!/usr/bin/env python3
"""Capture the reference note-edit operations as a golden table.

Run with the *reference* interpreter:

    /home/rg/.local/share/uv/tools/basic-memory/bin/python tools/dump_reference_edits.py

`basic_memory.services.note_preparation` implements the edit semantics as pure text
functions (`apply_edit_operation`, `replace_section_content`,
`insert_relative_to_section`, `_prepend_after_frontmatter`,
`_merge_metadata_into_markdown`), so a table of inputs and outputs pins the behavior
byte-for-byte without touching a database. `tests/note_golden.rs` replays it against
the Rust port.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]

BODY = "# Doc\n\nIntro line.\n\n## Alpha\n\nAlpha body.\n\n### Nested\n\nNested body.\n\n## Beta\n\nBeta body.\n"
FENCED = "# Doc\n\n```md\n## Alpha\n\nfenced body\n```\n\n## Alpha\n\nreal body\n"

# (id, current_content, operation, content, kwargs)
CASES: list[dict[str, Any]] = [
    {
        "id": "append-adds-newline",
        "content": "# Doc\n\nBody without trailing newline",
        "operation": "append",
        "payload": "Appended",
        "kwargs": {},
    },
    {
        "id": "append-with-frontmatter",
        "content": "---\ntitle: Doc\ntype: note\n---\n\nBody\n",
        "operation": "append",
        "payload": "Tail",
        "kwargs": {},
    },
    {
        "id": "prepend-after-frontmatter",
        "content": "---\ntitle: Doc\ntype: note\npermalink: notes/doc\n---\n\nBody\n",
        "operation": "prepend",
        "payload": "Opening paragraph.",
        "kwargs": {},
    },
    {
        "id": "prepend-without-frontmatter",
        "content": "Body only\n",
        "operation": "prepend",
        "payload": "Opening.",
        "kwargs": {},
    },
    {
        "id": "find-replace-single",
        "content": BODY,
        "operation": "find_replace",
        "payload": "Beta body replaced.",
        "kwargs": {"find_text": "Beta body.", "expected_replacements": 1},
    },
    {
        "id": "find-replace-all",
        "content": BODY,
        "operation": "find_replace",
        "payload": "line",
        "kwargs": {"find_text": "body", "expected_replacements": 2},
    },
    {
        "id": "replace-section",
        "content": BODY,
        "operation": "replace_section",
        "payload": "Replacement body.",
        "kwargs": {"section": "## Alpha"},
    },
    {
        "id": "replace-section-preserving-subsections",
        "content": BODY,
        "operation": "replace_section",
        "payload": "Replacement body.",
        "kwargs": {"section": "Alpha", "replace_subsections": False},
    },
    {
        "id": "replace-section-with-header-in-payload",
        "content": BODY,
        "operation": "replace_section",
        "payload": "## Alpha\n\nPayload body.",
        "kwargs": {"section": "Alpha"},
    },
    {
        "id": "replace-section-missing-appends",
        "content": BODY,
        "operation": "replace_section",
        "payload": "New section body.",
        "kwargs": {"section": "Gamma"},
    },
    {
        "id": "insert-after-section",
        "content": BODY,
        "operation": "insert_after_section",
        "payload": "Inserted after alpha.",
        "kwargs": {"section": "Alpha"},
    },
    {
        "id": "insert-before-section",
        "content": BODY,
        "operation": "insert_before_section",
        "payload": "Inserted before beta.",
        "kwargs": {"section": "Beta"},
    },
    {
        "id": "fenced-heading-is-not-a-section",
        "content": FENCED,
        "operation": "insert_after_section",
        "payload": "Inserted after the real heading.",
        "kwargs": {"section": "Alpha"},
    },
    {
        "id": "metadata-merge-adds-key",
        "content": "---\ntitle: Doc\ntype: note\n---\n\nBody\n",
        "operation": "metadata_merge",
        "payload": None,
        "kwargs": {"metadata": {"status": "active", "priority": 3}},
    },
    {
        "id": "metadata-merge-drops-identity-fields",
        "content": "---\ntitle: Doc\ntype: note\n---\n\nBody\n",
        "operation": "metadata_merge",
        "payload": None,
        "kwargs": {"metadata": {"title": "Other", "permalink": "x", "status": "done"}},
    },
    {
        "id": "metadata-merge-into-file-without-frontmatter",
        "content": "Body only",
        "operation": "metadata_merge",
        "payload": None,
        "kwargs": {"metadata": {"status": "active"}},
    },
]

# Error cases: the reference raises ValueError; the golden records the message.
ERROR_CASES: list[dict[str, Any]] = [
    {
        "id": "find-replace-missing-text",
        "content": BODY,
        "operation": "find_replace",
        "payload": "x",
        "kwargs": {"find_text": "not present", "expected_replacements": 1},
    },
    {
        "id": "find-replace-wrong-count",
        "content": BODY,
        "operation": "find_replace",
        "payload": "x",
        "kwargs": {"find_text": "body", "expected_replacements": 1},
    },
    {
        "id": "unsupported-operation",
        "content": BODY,
        "operation": "frobnicate",
        "payload": "x",
        "kwargs": {},
    },
    {
        "id": "section-missing-for-insert",
        "content": BODY,
        "operation": "insert_after_section",
        "payload": "x",
        "kwargs": {},
    },
    {
        "id": "insert-into-missing-section",
        "content": BODY,
        "operation": "insert_after_section",
        "payload": "x",
        "kwargs": {"section": "Gamma"},
    },
    {
        "id": "duplicate-section-header",
        "content": "## Alpha\n\none\n\n## Alpha\n\ntwo\n",
        "operation": "replace_section",
        "payload": "x",
        "kwargs": {"section": "Alpha"},
    },
]


def run_case(case: dict[str, Any]) -> dict[str, Any]:
    from basic_memory.services.note_preparation import (
        _merge_metadata_into_markdown,
        apply_edit_operation,
    )

    record: dict[str, Any] = {
        "id": case["id"],
        "content": case["content"],
        "operation": case["operation"],
        "payload": case["payload"],
        "kwargs": case["kwargs"],
    }
    try:
        if case["operation"] == "metadata_merge":
            result = _merge_metadata_into_markdown(
                case["content"], case["kwargs"]["metadata"]
            )
        else:
            result = apply_edit_operation(
                case["content"],
                case["operation"],
                case["payload"] or "",
                section=case["kwargs"].get("section"),
                find_text=case["kwargs"].get("find_text"),
                expected_replacements=case["kwargs"].get("expected_replacements", 1),
                replace_subsections=case["kwargs"].get("replace_subsections", True),
            )
        record["result"] = result
    except Exception as exc:  # noqa: BLE001 - record the reference failure verbatim
        record["error"] = f"{type(exc).__name__}: {exc}"
    return record


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=REPO_ROOT / "tests" / "golden" / "note" / "edit-operations.json",
    )
    args = parser.parse_args()

    cases = [run_case(case) for case in [*CASES, *ERROR_CASES]]
    payload = {
        "source": "basic_memory.services.note_preparation",
        "cases": cases,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, ensure_ascii=False, indent=1) + "\n")
    print(f"wrote {args.out} ({len(cases)} cases)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
