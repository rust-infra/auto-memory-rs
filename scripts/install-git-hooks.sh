#!/bin/sh
# Install the repository's git hooks into this clone's `.git/hooks`.
#
# Why not `git config core.hooksPath .githooks`: that points git at the *working
# tree*, so the hooks exist only while a branch containing them is checked out.
# Switching to a branch that predates them (or to `main` before the merge)
# silently disables every hook — including the guard that is supposed to stop you
# committing to `main`. Measured: a probe commit landed on `main` exactly that
# way.
#
# `.git/hooks` is outside the working tree, so it survives every checkout, merge,
# and rebase. This script copies `.githooks/*` there; re-run it after editing a
# hook.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SRC="$ROOT/.githooks"
DST=$(git -C "$ROOT" rev-parse --git-path hooks)
# `--git-path` may be relative to the repo root.
case "$DST" in
  /*) ;;
  *) DST="$ROOT/$DST" ;;
esac

[ -d "$SRC" ] || { echo "error: $SRC not found" >&2; exit 1; }
mkdir -p "$DST"

for hook in "$SRC"/*; do
  [ -f "$hook" ] || continue
  name=$(basename "$hook")
  cp "$hook" "$DST/$name"
  chmod +x "$DST/$name"
  echo "installed $name"
done

echo
echo "Hooks installed into $DST"
echo "Verify: git commit on main should be refused."
