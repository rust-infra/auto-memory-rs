#!/bin/sh
# Finish the auto-memory-rs setup for DeepSeek Harness *without* installing the
# bundle.
#
# The recommended path is `dsh plugin --profile desktop add dsh-plugin`, which
# ships zero-argument MCP args and needs only the config file below. This script
# is the **manual** alternative: it wires the pieces by hand, for a checkout that
# is not installed as a package, or to pin a specific vault/project.
#
# Do not run both for the same profile: the bundle's `insert:` rows and this
# script's `insert:` block would each add an `auto-memory-mcp`, and DSH rejects
# duplicate entry ids.
#
# Run this yourself: it writes outside the project workspace
# (~/.dsh/profiles/desktop/cordis.patch.yml, ~/.dsh/skills, ~/.config/auto-memory,
# $VAULT/.dsh), which a sandboxed agent session cannot reach.
#
#   sh scripts/wire-dsh-desktop.sh
#
# Overridable: VAULT=... PROJECT=... BIN=...
# Idempotent: re-running replaces the auto-memory insert block instead of
# stacking a second one. Backs up cordis.patch.yml before touching it.
set -eu

VAULT=${VAULT:-/Users/rg/agent-memory}
PROJECT=${PROJECT:-1m}
BIN=${BIN:-/Users/rg/.cargo/bin/auto-memory}
PATCH="$HOME/.dsh/profiles/desktop/cordis.patch.yml"
ROW_ID=mcp-auto-memory
HERE=$(cd "$(dirname "$0")" && pwd)
SKILLS=$(cd "$HERE/../plugins/deepseek/skills" && pwd)

[ -x "$BIN" ]        || { echo "error: $BIN is not executable" >&2; exit 1; }
[ -d "$VAULT" ]      || { echo "error: vault $VAULT is not a directory" >&2; exit 1; }
[ -d "$SKILLS" ]     || { echo "error: $SKILLS not found (run from the repo)" >&2; exit 1; }
[ -f "$PATCH" ]      || { echo "error: $PATCH not found (has the desktop app run once?)" >&2; exit 1; }

echo "==> 1/4 auto-memory config: default_project=$PROJECT"
# The MCP server resolves index and project from here, which is what lets the
# mounted row stay argument-free. `config set` owns the file format; writing the
# JSON by hand here would drift from it (and re-introduce every default key).
"$BIN" project default "$PROJECT"
if [ -n "${INDEX:-}" ]; then
  "$BIN" config set index "$INDEX"
fi
"$BIN" project default

echo "==> 2/4 project mapping (hook only): $VAULT/.dsh/auto-memory.json"
# Only the `auto-memory hook` front end reads this; DSH has no hook host today,
# so it is inert here. Written anyway so the two channels agree.
mkdir -p "$VAULT/.dsh"
cat > "$VAULT/.dsh/auto-memory.json" <<JSON
{
  "autoMemory": {
    "primaryProject": "$PROJECT"
  }
}
JSON

echo "==> 3/4 skills -> ~/.dsh/skills"
mkdir -p "$HOME/.dsh/skills"
for skill in "$SKILLS"/*; do
  ln -sfn "$skill" "$HOME/.dsh/skills/$(basename "$skill")"
done
ls "$HOME/.dsh/skills"

echo "==> 4/4 mcp row in $PATCH"
backup="$PATCH.bak-$(date +%Y%m%d-%H%M%S)"
cp "$PATCH" "$backup"
echo "    backup: $backup"

ROW_ID="$ROW_ID" BIN="$BIN" PROJECT="$PROJECT" PATCH="$PATCH" python3 - <<'PY'
import os, re, sys

patch = os.environ["PATCH"]
row_id = os.environ["ROW_ID"]
binary = os.environ["BIN"]
project = os.environ["PROJECT"]

lines = open(patch).read().splitlines()

# Remove a previously written `- insert:` block if it declares our row id.
# A block runs from its `- insert:` line through its last indented line.
out, i, removed = [], 0, 0
while i < len(lines):
    if lines[i].strip() == "- insert:":
        j = i + 1
        while j < len(lines) and (lines[j].startswith((" ", "\t")) or not lines[j].strip()):
            j += 1
        block = lines[i:j]
        if any(re.search(r"^\s*-\s*id:\s*%s\s*$" % re.escape(row_id), b) for b in block):
            i, removed = j, removed + 1
            continue
        out.extend(block)
        i = j
        continue
    out.append(lines[i])
    i += 1

if out and out[-1].strip():
    out.append("")

out.extend([
    "- insert:",
    "    - id: %s" % row_id,
    '      name: "@deepseek-ai/dsh-mcp-client"',
    "      config:",
    "        serverName: memory",
    "        transport: stdio",
    "        command: %s" % binary,
    '        args: ["mcp", "--project", "%s", "--read-only"]' % project,
    "        toolCallTimeoutMs: 60000",
    "        failOnStartupError: true",
])

open(patch, "w").write("\n".join(out) + "\n")
print("    replaced %d previous block(s)" % removed if removed else "    added")
PY

echo
echo "Done. Verify:"
echo "  tail -14 $PATCH"
echo "  DSH_HOME=\$HOME/.dsh dsh --profile web --patch $PATCH --dump-config | grep -A12 mcp-auto-memory"
echo
echo "The desktop profile itself is Electron-managed ('managed exclusively by the"
echo "Electron application'), so validate the patch as an overlay against another"
echo "profile rather than booting desktop from the CLI."
