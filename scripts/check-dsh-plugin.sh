#!/bin/sh
# Validate the `dsh-plugin/` bundle without needing DSH or npm installed.
#
# The bundle is a separate artifact from the Rust crate: it is published to npm
# and installed by `dsh plugin --profile <name> add`, so the things that can rot
# are its *files*, not its code. This checks the three that have broken before or
# would break silently:
#
#   1. skills/schemas still match `plugins/deepseek` (run
#      `scripts/sync-dsh-plugin.sh` when they drift);
#   2. `package.json` still declares the `dsh.bundle.patch` entry DSH reads to
#      find `cordis.patch.yml`, and every path in `files` exists;
#   3. `cordis.patch.yml` still uses `insert:` for rows that do not exist in any
#      bundle — a bare `- id:` row is rejected at load with
#      `entry "…" not found`, which is the failure this whole check exists for.
#
# Exit 0 when clean. Runs in CI and from `scripts/check-rust.sh`.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
PKG=dsh-plugin
fail=0

say() { printf '%s\n' "$*"; }
bad() { printf 'FAIL %s\n' "$*" >&2; fail=1; }

[ -d "$PKG" ] || { bad "$PKG/ is missing"; exit 1; }

# --- 1. skills/schemas in sync with the source package -----------------------
if sh scripts/sync-dsh-plugin.sh --check >/dev/null 2>&1; then
  say "ok   skills/schemas in sync with plugins/deepseek"
else
  bad "dsh-plugin/{skills,schemas} drifted from plugins/deepseek — run: sh scripts/sync-dsh-plugin.sh"
fi

# --- 2. package.json manifest ------------------------------------------------
manifest="$PKG/package.json"
if [ ! -f "$manifest" ]; then
  bad "$manifest is missing"
else
  # Read the fields with node when it is available (the bundle targets Node
  # anyway); fall back to grep so the check still runs on a Rust-only CI image.
  if command -v node >/dev/null 2>&1; then
    node -e '
      const fs = require("node:fs");
      const path = require("node:path");
      const dir = process.argv[1];
      const pkg = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
      const problems = [];
      const patch = pkg.dsh && pkg.dsh.bundle && pkg.dsh.bundle.patch;
      if (!patch) problems.push("dsh.bundle.patch is not declared");
      else if (!fs.existsSync(path.join(dir, patch))) problems.push(`dsh.bundle.patch ${patch} does not exist`);
      if (pkg.type !== "module") problems.push("type must be \"module\" (the bundle is ESM)");
      if (!Array.isArray(pkg.files) || pkg.files.length === 0) problems.push("files[] is empty");
      for (const entry of pkg.files || []) {
        if (!fs.existsSync(path.join(dir, entry))) problems.push(`files[] entry ${entry} does not exist`);
      }
      if (!fs.existsSync(path.join(dir, "lib/index.js"))) problems.push("lib/index.js is missing");
      if (problems.length) { console.error(problems.join("\n")); process.exit(1); }
    ' "$PKG" || bad "$manifest failed validation"
    [ "$fail" -eq 0 ] && say "ok   package.json declares dsh.bundle.patch and a complete files[]"
  else
    grep -q '"patch"' "$manifest" || bad "dsh.bundle.patch is not declared"
    [ -f "$PKG/cordis.patch.yml" ] || bad "$PKG/cordis.patch.yml is missing"
    [ -f "$PKG/lib/index.js" ] || bad "$PKG/lib/index.js is missing"
    say "ok   package.json has a patch entry (node unavailable; shallow check)"
  fi
fi

# --- 3. cordis.patch.yml uses insert: for new rows ---------------------------
patch="$PKG/cordis.patch.yml"
if [ ! -f "$patch" ]; then
  bad "$patch is missing"
else
  # Every row this bundle introduces must live under an `insert:` block. A
  # top-level `- id:` row targets an existing entry and fails at load.
  top_level_ids=$(grep -c '^- id:' "$patch" || true)
  if [ "$top_level_ids" -ne 0 ]; then
    bad "$patch has $top_level_ids top-level '- id:' row(s); a new plugin row must be under '- insert:'"
  else
    say "ok   cordis.patch.yml adds rows only via insert:"
  fi
  inserts=$(grep -c '^- insert:' "$patch" || true)
  if [ "$inserts" -eq 0 ]; then
    bad "$patch declares no '- insert:' block"
  fi
  # The two rows this bundle is expected to contribute. Missing one is not
  # fatal to the bundle (it still installs) but means half the integration is
  # gone, which is worth a red gate.
  for row in auto-memory-skill-filesystem auto-memory-mcp; do
    grep -q "id: $row" "$patch" || bad "$patch no longer declares the '$row' row"
  done
  say "ok   cordis.patch.yml declares both expected rows"

  # Zero-argument contract: the shipped args must not name a machine-specific
  # vault, index, or project. `auto-memory mcp` resolves all three from
  # `~/.config/auto-memory/config.json`, and a baked-in value would silently
  # point every install at the author's machine.
  # Only the *effective* args line counts; the file's comments show override
  # examples that name these flags on purpose.
  if grep -vE '^\s*#' "$patch" | grep -qE '^\s*args:.*--(project|vault|index)'; then
    bad "$patch ships machine-specific args; leave them to the user's config file"
  else
    say "ok   shipped mcp args carry no vault/index/project"
  fi

  # The server must be launchable without editing: `command: auto-memory` has to
  # resolve through PATH (dsh-subprocess keeps PATH when it scrubs the env).
  grep -qE '^\s*command: auto-memory\s*$' "$patch" \
    || bad "$patch should ship 'command: auto-memory' (PATH-resolved), not an absolute path"
  say "ok   command resolves through PATH"
fi

if [ "$fail" -ne 0 ]; then
  echo
  echo "dsh-plugin check failed." >&2
  exit 1
fi
echo "dsh-plugin check passed."
