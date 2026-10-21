#!/bin/sh
# Copy the DeepSeek package's skills/schemas into the DSH bundle.
#
# The bundle is published to npm, so it cannot reference `../../plugins/...`
# (npm packs only what `files` lists, and a `file:` dependency on the parent
# repo would drag the whole crate along). One source of truth, copied at pack
# time, checked by `--check`.
#
#   sh scripts/sync-dsh-plugin.sh          # copy
#   sh scripts/sync-dsh-plugin.sh --check  # fail if out of sync (CI)
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SRC="$ROOT/plugins/deepseek"
DST="$ROOT/dsh-plugin"

for sub in skills schemas; do
  [ -d "$SRC/$sub" ] || { echo "error: $SRC/$sub missing" >&2; exit 1; }
done

if [ "${1:-}" = "--check" ]; then
  drift=0
  for sub in skills schemas; do
    if ! diff -r "$SRC/$sub" "$DST/$sub" >/dev/null 2>&1; then
      echo "drift: dsh-plugin/$sub differs from plugins/deepseek/$sub" >&2
      diff -rq "$SRC/$sub" "$DST/$sub" >&2 || true
      drift=1
    fi
  done
  [ "$drift" -eq 0 ] || { echo "run: sh scripts/sync-dsh-plugin.sh" >&2; exit 1; }
  echo "dsh-plugin skills/schemas are in sync"
  exit 0
fi

for sub in skills schemas; do
  rm -rf "$DST/$sub"
  cp -R "$SRC/$sub" "$DST/$sub"
done
echo "synced plugins/deepseek/{skills,schemas} -> dsh-plugin/"
