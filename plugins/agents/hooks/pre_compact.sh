#!/bin/sh
# Thin launcher for the Codex PreCompact hook.
#
# The logic lives in `auto-memory hook pre-compact`. Codex ignores PreCompact
# stdout; the checkpoint request is delivered by the post-compaction
# SessionStart hook instead. Fail-open: a missing binary or any non-zero exit
# is swallowed.
bin="${AUTO_MEMORY_BIN:-auto-memory}"
command -v "$bin" >/dev/null 2>&1 || exit 0
"$bin" hook pre-compact --harness codex || exit 0
exit 0
