#!/bin/sh
# Thin launcher for the Codex SessionStart hook.
#
# The logic lives in `auto-memory hook session-start`; this script only bridges
# the plugin contract to it. Fail-open: a hook must never disrupt a session, so
# a missing binary or any non-zero exit is swallowed. stdout carries the brief
# only; diagnostics go to stderr.
bin="${AUTO_MEMORY_BIN:-auto-memory}"
command -v "$bin" >/dev/null 2>&1 || exit 0
"$bin" hook session-start --harness codex || exit 0
exit 0
