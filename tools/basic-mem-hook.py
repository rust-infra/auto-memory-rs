#!/usr/bin/env python3
"""Brief an agent session from a `basic-mem-rs` index.

Written as an agent-lifecycle hook for both harnesses that use the Claude Code
plugin contract — Codex plugins (`.codex-plugin/plugin.json` → `hooks/hooks.json`)
and Tact plugins (`tact-ui plugin install`) — because the contract is the same:

    stdin : one JSON object  (session_id, transcript_path, cwd, hook_event_name, …)
    stdout: one JSON object  ({"hookSpecificOutput": {"additionalContext": …}})
            or, for SessionStart / UserPromptSubmit, plain text (Tact treats it as
            the context verbatim, the reference implementation does the same)
    exit  : always 0 — a hook must never break the session

What it does:

* `SessionStart`      → the notes touched in the last `--days` window.
* `UserPromptSubmit`  → the notes matching the prompt (a retrieval hint, not a
                        filter: the agent still decides what to read).

Configuration is environment-only, so the same script works for any vault:

    BASIC_MEM_INDEX    index file (default: ~/.local/share/basic-mem/memory.db)
    BASIC_MEM_PROJECT  project permalink, e.g. `oracle`          (required)
    BASIC_MEM_BIN      binary (default: `basic-mem` from PATH)
    BASIC_MEM_DAYS     lookback window for SessionStart (default: 7)
    BASIC_MEM_LIMIT    results per briefing (default: 8)
    BASIC_MEM_QUERY    replaces the SessionStart lookback with a search query

Project mapping per directory: set `BASIC_MEM_PROJECT_<SLUG>` where `<SLUG>` is the
payload `cwd` upper-cased with every non-alphanumeric character replaced by `_`
(e.g. `/home/me/vault` → `BASIC_MEM_PROJECT_HOME_ME_VAULT`), and it wins over
`BASIC_MEM_PROJECT`. That is how one hook serves several vaults.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys


def payload() -> dict:
    """The harness payload, tolerating an empty or malformed stdin."""
    try:
        raw = sys.stdin.read()
    except OSError:
        return {}
    try:
        value = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError:
        return {}
    return value if isinstance(value, dict) else {}


def slug(path: str) -> str:
    """Environment-variable fragment for a directory: `/home/me/vault` → `HOME_ME_VAULT`."""
    return re.sub(r"[^A-Za-z0-9]", "_", path).strip("_").upper()


def setting(name: str, cwd: str, default: str | None = None) -> str | None:
    """`BASIC_MEM_<name>_<cwd slug>` if set, else `BASIC_MEM_<name>`."""
    if cwd:
        scoped = os.environ.get(f"BASIC_MEM_{name}_{slug(cwd)}")
        if scoped:
            return scoped
    return os.environ.get(f"BASIC_MEM_{name}", default)


def run_search(args: list[str], binary: str, timeout: float = 8.0) -> dict:
    """Run a `search`/`status` command and parse its JSON, or return nothing.

    Every failure path degrades to "no briefing": a missing index or a slow disk
    must not turn into a broken session.
    """
    try:
        completed = subprocess.run(
            [binary, *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return {}
    if completed.returncode != 0:
        return {}
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError:
        return {}


def brief_rows(page: dict) -> list[str]:
    rows = []
    for result in page.get("results", []):
        title = result.get("title") or result.get("permalink") or "?"
        permalink = result.get("permalink") or result.get("entity") or ""
        rows.append(f"- {title} ({permalink})")
    return rows


def build(session: dict) -> str:
    event = session.get("hook_event_name") or "SessionStart"
    cwd = session.get("cwd") or os.getcwd()
    binary = setting("BIN", cwd, "basic-mem") or "basic-mem"
    index = setting("INDEX", cwd) or os.path.expanduser(
        "~/.local/share/basic-mem/memory.db"
    )
    project = setting("PROJECT", cwd)
    if not project:
        return ""
    limit = setting("LIMIT", cwd, "8") or "8"

    common = ["--index", index, "--project", project, "--page-size", limit]

    if event == "UserPromptSubmit":
        query = (session.get("prompt") or "").strip()
        if not query:
            return ""
        page = run_search(["search", *common, query], binary)
        rows = brief_rows(page)
        if not rows:
            return ""
        return "Related notes in the memory index:\n" + "\n".join(rows)

    query = setting("QUERY", cwd)
    if query:
        page = run_search(["search", *common, query], binary)
        heading = f"Notes matching {query!r} in the memory index:"
    else:
        days = setting("DAYS", cwd, "7") or "7"
        page = run_search(["search", *common, "--after-date", f"{days}d"], binary)
        heading = f"Notes changed in the last {days} days:"
    rows = brief_rows(page)
    if not rows:
        return ""
    return heading + "\n" + "\n".join(rows)


def main() -> int:
    session = payload()
    # No payload means no event to answer: stay silent rather than guess "SessionStart".
    if not session.get("hook_event_name"):
        return 0
    try:
        context = build(session)
    except Exception:  # noqa: BLE001 - the fail-open boundary
        return 0
    if context:
        json.dump(
            {
                "hookSpecificOutput": {
                    "hookEventName": session.get("hook_event_name") or "SessionStart",
                    "additionalContext": context,
                }
            },
            sys.stdout,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
