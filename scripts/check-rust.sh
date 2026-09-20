#!/usr/bin/env bash
# The gates `.github/workflows/ci.yml` runs, runnable locally: formatting, lints, the
# full test suite, and the doc build. `.githooks/pre-push` calls this so a push is
# checked before it leaves the machine instead of after.
set -euo pipefail

# Git exports GIT_DIR / GIT_WORK_TREE into hook processes. Clear them so cargo — and the
# git commands some tests run against their own temporary repositories — resolve the
# repository from their cwd instead of operating on the hook's repository context.
unset GIT_DIR GIT_WORK_TREE

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found on PATH" >&2
  exit 1
fi

# A proxy pointing at this machine breaks the tests that drive a mock HTTP server
# (`tests/mcp_http.rs`, `tests/async_transport.rs`): the request leaves through the proxy
# and never comes back. The loopback addresses never go through one — the same value
# `.github/workflows/ci.yml` sets. Existing entries are kept, and both spellings are
# exported because HTTP clients disagree on which they read.
export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
export NO_PROXY="$no_proxy"

echo "==> cargo fmt --all -- --check"
cargo fmt --all -- --check

echo "==> cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

echo "==> cargo test --all-targets --locked"
cargo test --all-targets --locked

echo "==> cargo doc --no-deps --locked"
cargo doc --no-deps --locked

echo "All checks passed."
