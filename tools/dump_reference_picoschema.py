#!/usr/bin/env python3
"""Capture the reference Picoschema algorithms as a golden table.

Run with the *reference* interpreter:

    ~/.local/share/uv/tools/basic-memory/bin/python tools/dump_reference_picoschema.py

`basic_memory.picoschema` is pure Python over plain dicts: the parser turns a
frontmatter `schema` mapping into fields, the validator compares a note's
observations/relations against those fields, the inference engine derives a schema
from note frequency, and the diff engine reports drift. None of that touches a
database, so a table of inputs and outputs pins the behaviour exactly.

`tests/schema_golden.rs` replays the table against the Rust port.
"""

from __future__ import annotations

import argparse
import asyncio
import json
from dataclasses import asdict, is_dataclass
from pathlib import Path
from typing import Any

from basic_memory.picoschema import (
    NoteData,
    ObservationData,
    RelationData,
    diff_schema,
    infer_schema,
    parse_picoschema,
    parse_schema_note,
    resolve_schema,
    validate_note,
)

REPO_ROOT = Path(__file__).resolve().parents[1]


def to_json(value: Any) -> Any:
    """Convert dataclass results (and their nested fields) to plain JSON."""
    if is_dataclass(value) and not isinstance(value, type):
        return {key: to_json(item) for key, item in asdict(value).items()}
    if isinstance(value, dict):
        return {key: to_json(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [to_json(item) for item in value]
    return value


# --- Parser cases -----------------------------------------------------------

PARSE_CASES: list[dict[str, Any]] = [
    {
        "id": "scalars-and-optional",
        "schema": {
            "name": "string, full name",
            "role?": "string",
            "age": "integer",
            "score": "number",
            "active?": "boolean",
            "extra": "any",
        },
    },
    {
        "id": "array-modifier",
        "schema": {
            "tags?(array)": "string",
            "labels(array, free-form labels)": "string",
            "aliases?(array)": "string, other names",
        },
    },
    {
        "id": "entity-refs",
        "schema": {
            "employer?": "Organization, where they work",
            "works_at": "Company",
            "nickname": "string",
        },
    },
    {
        "id": "enum-as-list",
        "schema": {"status?(enum)": ["active", "blocked", "done"]},
    },
    {
        "id": "enum-as-quoted-string",
        "schema": {"status?(enum)": "[active, blocked, done], current state"},
    },
    {
        "id": "enum-single-value",
        "schema": {"flag?(enum)": "on"},
    },
    {
        "id": "object-nested",
        "schema": {
            "metadata?(object)": {"source": "string", "confidence?": "number"},
            "plain-object": {"label": "string"},
        },
    },
    {
        "id": "parentheses-in-names-and-descriptions",
        "schema": {
            "risk(score)": "string",
            "notes?(array, freeform (no format))": "string",
        },
    },
    {
        "id": "empty-schema",
        "schema": {},
    },
]

# --- Schema-note cases ------------------------------------------------------

SCHEMA_NOTE_CASES: list[dict[str, Any]] = [
    {
        "id": "person-warn",
        "frontmatter": {
            "title": "Person",
            "type": "schema",
            "entity": "person",
            "version": 2,
            "schema": {"name": "string", "employer?": "Organization"},
            "settings": {"validation": "warn"},
        },
    },
    {
        "id": "person-strict",
        "frontmatter": {
            "entity": "person",
            "schema": {"name": "string"},
            "settings": {"validation": "strict"},
        },
    },
    {
        "id": "error-alias",
        "frontmatter": {
            "entity": "person",
            "schema": {"name": "string"},
            "settings": {"validation": "error"},
        },
    },
    {
        "id": "defaults",
        "frontmatter": {"entity": "person", "schema": {"name": "string"}},
    },
    {
        "id": "frontmatter-settings",
        "frontmatter": {
            "entity": "person",
            "schema": {"name": "string"},
            "settings": {
                "validation": "warn",
                "frontmatter": {
                    "status?(enum)": ["active", "archived"],
                    "tags?(array)": "string",
                },
            },
        },
    },
]

SCHEMA_NOTE_ERROR_CASES: list[dict[str, Any]] = [
    {"id": "missing-entity", "frontmatter": {"schema": {"name": "string"}}},
    {"id": "missing-schema", "frontmatter": {"entity": "person"}},
    {"id": "schema-not-a-dict", "frontmatter": {"entity": "person", "schema": "Person"}},
    {
        "id": "invalid-validation",
        "frontmatter": {
            "entity": "person",
            "schema": {"name": "string"},
            "settings": {"validation": "loud"},
        },
    },
]

# --- Validation cases -------------------------------------------------------

PERSON_SCHEMA = {
    "entity": "person",
    "schema": {
        "name": "string",
        "role?": "string",
        "status?(enum)": ["active", "archived"],
        "tags?(array)": "string",
        "employer?": "Organization",
    },
    "settings": {
        "validation": "warn",
        "frontmatter": {"status?(enum)": ["active", "archived"]},
    },
}

STRICT_PERSON_SCHEMA = {
    "entity": "person",
    "schema": PERSON_SCHEMA["schema"],
    "settings": {"validation": "strict"},
}


def note(**kwargs: Any) -> dict[str, Any]:
    base = {"identifier": kwargs.pop("identifier", "people/ada"), "observations": [], "relations": []}
    base.update(kwargs)
    return base


def obs(category: str, content: str) -> dict[str, str]:
    return {"category": category, "content": content}


def rel(relation_type: str, target_name: str, target_note_type: str | None = None) -> dict[str, Any]:
    return {
        "relation_type": relation_type,
        "target_name": target_name,
        "target_note_type": target_note_type,
    }


VALIDATE_CASES: list[dict[str, Any]] = [
    {
        "id": "fully-valid",
        "schema": PERSON_SCHEMA,
        "note": note(
            observations=[
                obs("name", "Ada Lovelace"),
                obs("role", "Mathematician"),
                obs("status", "active"),
                obs("tags", "math"),
                obs("tags", "history"),
            ],
            relations=[rel("employer", "Analytical Engine", "organization")],
        ),
        "frontmatter": {"status": "active"},
    },
    {
        "id": "missing-required",
        "schema": PERSON_SCHEMA,
        "note": note(observations=[obs("role", "Mathematician")]),
        "frontmatter": None,
    },
    {
        "id": "enum-mismatch",
        "schema": PERSON_SCHEMA,
        "note": note(observations=[obs("name", "Ada"), obs("status", "retired")]),
        "frontmatter": None,
    },
    {
        "id": "unmatched-content",
        "schema": PERSON_SCHEMA,
        "note": note(
            observations=[obs("name", "Ada"), obs("hobby", "poetry"), obs("hobby", "chess")],
            relations=[rel("mentor", "Charles Babbage")],
        ),
        "frontmatter": None,
    },
    {
        "id": "strict-promotes-to-error",
        "schema": STRICT_PERSON_SCHEMA,
        "note": note(observations=[obs("role", "Mathematician")]),
        "frontmatter": None,
    },
    {
        "id": "frontmatter-missing-and-mismatch",
        "schema": PERSON_SCHEMA,
        "note": note(observations=[obs("name", "Ada")]),
        "frontmatter": {"status": "retired"},
    },
    {
        "id": "frontmatter-array-values",
        "schema": PERSON_SCHEMA,
        "note": note(observations=[obs("name", "Ada")]),
        "frontmatter": {"status": "active", "tags": ["math", "history"]},
    },
]

# --- Inference cases --------------------------------------------------------

INFER_NOTES: dict[str, list[dict[str, Any]]] = {
    "empty": [],
    "uniform": [
        note(identifier="a", observations=[obs("name", "A"), obs("role", "R")], relations=[rel("employer", "Org", "organization")]),
        note(identifier="b", observations=[obs("name", "B"), obs("role", "S")], relations=[rel("employer", "Org2", "organization")]),
    ],
    "mixed-frequency": [
        note(identifier="a", observations=[obs("name", "A"), obs("status", "active"), obs("tags", "x"), obs("tags", "y")]),
        note(identifier="b", observations=[obs("name", "B"), obs("status", "archived")]),
        note(identifier="c", observations=[obs("name", "C")]),
        note(identifier="d", observations=[obs("name", "D"), obs("rare", "once")]),
    ],
    "relations-mixed-targets": [
        note(identifier="a", relations=[rel("knows", "X", "person"), rel("knows", "Y", "person"), rel("knows", "Z", "organization")]),
        note(identifier="b", relations=[rel("knows", "W", "person")]),
        note(identifier="c", relations=[rel("works_at", "Org", None)]),
    ],
}

# --- Diff cases -------------------------------------------------------------

DIFF_SCHEMA = {
    "entity": "person",
    "schema": {
        "name": "string",
        "status?": "string",
        "tags?(array)": "string",
        "employer?": "Organization",
    },
}

DIFF_NOTES: dict[str, list[dict[str, Any]]] = {
    "no-drift": [
        note(identifier="a", observations=[obs("name", "A"), obs("status", "active"), obs("tags", "x"), obs("tags", "y")]),
        note(identifier="b", observations=[obs("name", "B"), obs("status", "archived")]),
        note(identifier="c", observations=[obs("name", "C"), obs("status", "active")]),
    ],
    "new-and-dropped": [
        note(identifier="a", observations=[obs("name", "A"), obs("team", "core"), obs("team", "infra")]),
        note(identifier="b", observations=[obs("name", "B"), obs("team", "core")]),
        note(identifier="c", observations=[obs("name", "C"), obs("status", "active")]),
        note(identifier="d", observations=[obs("name", "D")]),
    ],
    "cardinality-changes": [
        note(identifier="a", observations=[obs("name", "A"), obs("status", "one"), obs("status", "two")]),
        note(identifier="b", observations=[obs("name", "B")]),
        note(identifier="c", observations=[obs("name", "C"), obs("tags", "solo")]),
        note(identifier="d", observations=[obs("name", "D")]),
    ],
    "empty": [],
}

# --- Resolver cases ---------------------------------------------------------

RESOLVE_SCHEMA_NOTES = [
    {
        "title": "Person",
        "type": "schema",
        "entity": "person",
        "schema": {"name": "string"},
    }
]

RESOLVE_CASES: list[dict[str, Any]] = [
    {"id": "inline", "frontmatter": {"type": "person", "schema": {"name": "string"}}},
    {"id": "explicit-reference", "frontmatter": {"type": "person", "schema": "person"}},
    {"id": "implicit-by-type", "frontmatter": {"type": "person"}},
    {"id": "no-schema", "frontmatter": {"type": "unrelated"}},
    {"id": "no-type", "frontmatter": {}},
]


def parse_schema_case(case: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": case["id"],
        "schema": case["schema"],
        "fields": to_json(parse_picoschema(case["schema"])),
    }


def parse_note_case(case: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": case["id"],
        "frontmatter": case["frontmatter"],
        "definition": to_json(parse_schema_note(case["frontmatter"])),
    }


def parse_note_error_case(case: dict[str, Any]) -> dict[str, Any]:
    try:
        parse_schema_note(case["frontmatter"])
    except ValueError as error:
        return {"id": case["id"], "frontmatter": case["frontmatter"], "error": str(error)}
    raise AssertionError(f"{case['id']} did not raise")


def validate_case(case: dict[str, Any]) -> dict[str, Any]:
    definition = parse_schema_note(case["schema"])
    raw = case["note"]
    result = validate_note(
        note_identifier=raw["identifier"],
        schema=definition,
        observations=[ObservationData(**item) for item in raw["observations"]],
        relations=[RelationData(**item) for item in raw["relations"]],
        frontmatter=case["frontmatter"],
    )
    return {"id": case["id"], "schema": case["schema"], "note": raw,
            "frontmatter": case["frontmatter"], "result": to_json(result)}


def infer_case(key: str, notes: list[dict[str, Any]]) -> dict[str, Any]:
    result = infer_schema(key, [note_from(raw) for raw in notes])
    return {"id": key, "notes": notes, "result": to_json(result)}


def diff_case(key: str, notes: list[dict[str, Any]]) -> dict[str, Any]:
    definition = parse_schema_note(DIFF_SCHEMA)
    result = diff_schema(definition, [note_from(raw) for raw in notes])
    return {"id": key, "schema": DIFF_SCHEMA, "notes": notes, "result": to_json(result)}


def note_from(raw: dict[str, Any]) -> NoteData:
    return NoteData(
        identifier=raw["identifier"],
        observations=[ObservationData(**item) for item in raw["observations"]],
        relations=[RelationData(**item) for item in raw["relations"]],
    )


async def resolve_case(case: dict[str, Any]) -> dict[str, Any]:
    async def search_fn(query: str) -> list[dict[str, Any]]:
        return [
            item
            for item in RESOLVE_SCHEMA_NOTES
            if item["entity"] == query or item["title"].casefold() == query.casefold()
        ]

    definition = await resolve_schema(case["frontmatter"], search_fn)
    return {
        "id": case["id"],
        "frontmatter": case["frontmatter"],
        "definition": to_json(definition) if definition else None,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=REPO_ROOT / "tests" / "golden" / "schema" / "picoschema.json",
    )
    args = parser.parse_args()

    payload = {
        "source": "basic_memory.picoschema",
        "parse": [parse_schema_case(case) for case in PARSE_CASES],
        "parse_schema_note": [parse_note_case(case) for case in SCHEMA_NOTE_CASES],
        "parse_schema_note_errors": [parse_note_error_case(case) for case in SCHEMA_NOTE_ERROR_CASES],
        "validate": [validate_case(case) for case in VALIDATE_CASES],
        "infer": [infer_case(key, notes) for key, notes in INFER_NOTES.items()],
        "diff": [diff_case(key, notes) for key, notes in DIFF_NOTES.items()],
        "resolve": [asyncio.run(resolve_case(case)) for case in RESOLVE_CASES],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, ensure_ascii=False, indent=1) + "\n")
    total = sum(len(payload[key]) for key in ("parse", "parse_schema_note", "parse_schema_note_errors",
                                              "validate", "infer", "diff", "resolve"))
    print(f"wrote {args.out} ({total} cases)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
