#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder — Launch Script for STDIO Sidecar Daemon
# ────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/thunder-agent-daemon"

if ! command -v cargo &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    elif [ -f "$HOME/.cargo/bin/cargo" ]; then
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        echo "Error: Cargo / Rust is not installed or not in PATH."
        exit 1
    fi
fi

exec cargo run --bin thunder-daemon -- "$@"
