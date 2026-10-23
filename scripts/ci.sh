#!/usr/bin/env bash
# Local mirror of .github/workflows/ci.yml — run before every push.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if ! command -v cargo &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    else
        echo "error: cargo not found in PATH" >&2
        exit 1
    fi
fi

step() {
    printf '\n\033[1m▸ %s\033[0m\n' "$*"
}

step "cargo fmt --all -- --check"
cargo fmt --all -- --check

step "cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo test --workspace"
cargo test --workspace

printf '\n\033[32m✓ 全部通过\033[0m\n'
