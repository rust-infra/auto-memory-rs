#!/usr/bin/env bash
#
# Install the `auto-memory` binary from a GitHub release.
#
#   ./scripts/install.sh                     # latest release
#   ./scripts/install.sh --version v0.1.0
#   ./scripts/install.sh --prefix /usr/local # default: ~/.local/bin
#
# A release asset is downloadable anonymously when the repository is public. When it is
# not, set GITHUB_TOKEN (or GH_TOKEN) to a token with `Contents: Read`: the script then
# resolves the asset through the API instead, which is the only route that works for a
# private repository. Without a token it uses the public download URL and reports the
# failure rather than guessing.
#
# Only the binary ships. ONNX Runtime is resolved at run time (see
# `.github/workflows/release.yml`), so semantic search needs it installed separately —
# `auto-memory doctor` reports whether it was found.

set -euo pipefail

REPO="${AUTO_MEMORY_REPO:-rust-infra/auto-memory-rs}"
PREFIX="${PREFIX:-$HOME/.local/bin}"
VERSION=""
DRY_RUN=0

usage() {
    # Everything between the shebang and the first blank line is the header; reading it
    # by pattern rather than by line number means editing the comment cannot silently
    # truncate `--help`. That blank line is included, so the heredoc does not repeat it.
    sed -n '3,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    cat <<'EOF'
Options:
  --version <tag>   Release tag to install (default: the latest release)
  --prefix <dir>    Where to put the binary (default: $PREFIX or ~/.local/bin)
  --repo <owner/repo>
  --dry-run         Print what would happen, download nothing
  -h, --help        This message
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:?--version needs a tag}"; shift 2 ;;
        --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
        --repo) REPO="${2:?--repo needs owner/name}"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

die() { echo "install: $*" >&2; exit 1; }

# --- platform -----------------------------------------------------------------
#
# The asset names match the target triples the release workflow builds, so the detection
# here is the same mapping the workflow's matrix uses.
detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux) os_part="unknown-linux-gnu" ;;
        Darwin) os_part="apple-darwin" ;;
        *) die "unsupported OS '$os'; download the release archive by hand" ;;
    esac
    case "$arch" in
        x86_64|amd64) arch_part="x86_64" ;;
        aarch64|arm64) arch_part="aarch64" ;;
        *) die "unsupported architecture '$arch'; download the release archive by hand" ;;
    esac
    TARGET="${arch_part}-${os_part}"
}

# --- download -----------------------------------------------------------------
#
# `curl` is assumed present: this script exists for the manual install path, and every
# platform it supports ships curl.
download() {
    local url="$1" out="$2" accept="${3:-}"
    local -a headers=()
    [ -n "${TOKEN:-}" ] && headers+=(-H "Authorization: Bearer $TOKEN")
    [ -n "$accept" ] && headers+=(-H "Accept: $accept")
    curl -fsSL "${headers[@]}" -o "$out" "$url"
}

# The tag of the newest release.
#
# With a token this is the API, which is also the only thing that works for a private
# repository. Without one it is the `/releases/latest` redirect, which GitHub only
# follows for public repositories.
latest_tag() {
    if [ -n "${TOKEN:-}" ]; then
        curl -fsSL -H "Authorization: Bearer $TOKEN" \
            "https://api.github.com/repos/$REPO/releases/latest" |
            sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1
    else
        local effective
        effective="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
            "https://github.com/$REPO/releases/latest" 2>/dev/null || true)"
        # No redirect means no release (or a private repository): the effective URL is
        # still `…/releases/latest`, and its last segment is not a tag.
        case "$effective" in
            */releases/latest|'') return 0 ;;
        esac
        printf '%s' "${effective##*/}"
    fi
}

# Release asset URLs for `$ASSET`.
#
# A private repository 404s on the `releases/download` path, so with a token the asset is
# resolved through the API and fetched by id with `Accept: application/octet-stream`.
asset_url() {
    local asset="$1"
    if [ -n "${TOKEN:-}" ]; then
        local id
        id="$(curl -fsSL -H "Authorization: Bearer $TOKEN" \
            "https://api.github.com/repos/$REPO/releases/tags/$VERSION" |
            python3 -c '
import json, sys
want = sys.argv[1]
for asset in json.load(sys.stdin).get("assets", []):
    if asset["name"] == want:
        print(asset["id"])
        break
' "$asset")"
        [ -n "$id" ] || return 1
        printf 'https://api.github.com/repos/%s/releases/assets/%s' "$REPO" "$id"
    else
        printf 'https://github.com/%s/releases/download/%s/%s' "$REPO" "$VERSION" "$asset"
    fi
}

# --- main ---------------------------------------------------------------------
detect_target
TOKEN="${GITHUB_TOKEN:-${GH_TOKEN:-}}"

if [ -z "$VERSION" ]; then
    VERSION="$(latest_tag)"
    [ -n "$VERSION" ] || die "could not resolve the latest release of $REPO (no release yet, or a private repository without GITHUB_TOKEN — or pass --version)"
fi

ASSET="auto-memory-${TARGET}.tar.gz"

echo "repository: $REPO"
echo "release:    $VERSION"
echo "target:     $TARGET"
echo "asset:      $ASSET"
echo "install to: $PREFIX/auto-memory"
[ -n "$TOKEN" ] || echo "token:      none (public download path)"

if [ "$DRY_RUN" = 1 ]; then
    exit 0
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

echo "downloading…"
download "$(asset_url "$ASSET")" "$workdir/$ASSET" "application/octet-stream" ||
    die "could not download $ASSET (404 — for a private repository, set GITHUB_TOKEN)"
download "$(asset_url SHA256SUMS)" "$workdir/SHA256SUMS" "application/octet-stream" ||
    die "could not download SHA256SUMS"

# Verify before anything is unpacked or installed.
#
# The checksum file covers every platform's archive, so only this one's line is used.
# That also keeps the check portable: macOS has no `sha256sum`, and `shasum -c` has no
# `--ignore-missing` for the archives that were not downloaded.
digest() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "no sha256sum or shasum available to verify the download"
    fi
}

expected="$(awk -v a="./$ASSET" -v b="$ASSET" '$2 == a || $2 == b { print $1 }' \
    "$workdir/SHA256SUMS")"
[ -n "$expected" ] || die "SHA256SUMS has no entry for $ASSET"
actual="$(digest "$workdir/$ASSET")"
[ "$expected" = "$actual" ] ||
    die "checksum mismatch for $ASSET (expected $expected, got $actual)"

tar -xzf "$workdir/$ASSET" -C "$workdir"
staged="$(find "$workdir" -maxdepth 2 -type f -name auto-memory -print -quit)"
[ -n "$staged" ] || die "$ASSET did not contain an auto-memory binary"

mkdir -p "$PREFIX"
# Install by rename so a partially written file is never on PATH.
install -m 0755 "$staged" "$PREFIX/.auto-memory.new"
mv -f "$PREFIX/.auto-memory.new" "$PREFIX/auto-memory"

echo "installed $("$PREFIX/auto-memory" --version)"
case ":$PATH:" in
    *":$PREFIX:"*) ;;
    *) echo "note: $PREFIX is not on PATH — add it, e.g. export PATH=\"$PREFIX:\$PATH\"" ;;
esac
