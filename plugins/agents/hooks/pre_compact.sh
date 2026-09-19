#!/bin/sh
# Thin launcher for the Codex PreCompact hook.
#
# The logic lives in `basic-mem hook pre-compact`. Codex ignores PreCompact
# stdout; the checkpoint request is delivered by the post-compaction
# SessionStart hook instead. Fail-open: a missing binary or any non-zero exit
# is swallowed.
bin="${BASIC_MEM_BIN:-basic-mem}"
command -v "$bin" >/dev/null 2>&1 || exit 0
"$bin" hook pre-compact --harness codex || exit 0
exit 0
