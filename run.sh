#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder — Root Launch Script for Interactive TUI
# ────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/thunder-agent-core"

if ! command -v cargo &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    else
        echo "Error: Cargo / Rust is not installed or not in PATH."
        exit 1
    fi
fi

exec cargo run -p thunder-tui --bin thunder-tui -- "$@"
