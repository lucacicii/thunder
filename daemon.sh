#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder — Launch Script for STDIO Sidecar Daemon
#
# The daemon falls back to its current directory as the default
# workspace, so this script preserves the caller's cwd. The build
# step cd's into the cargo workspace inside a subshell only.
# All build chatter goes to stderr: stdout is the STDIO protocol.
# ────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v cargo &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    elif [ -f "$HOME/.cargo/bin/cargo" ]; then
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        echo "Error: Cargo / Rust is not installed or not in PATH." >&2
        exit 1
    fi
fi

( cd "$SCRIPT_DIR" && cargo build -p thunder-agent-daemon --bin thunder-daemon >&2 )

TARGET_DIR="${CARGO_TARGET_DIR:-$SCRIPT_DIR/target}"
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$SCRIPT_DIR/$TARGET_DIR" ;;
esac
BIN="$TARGET_DIR/debug/thunder-daemon"

if [ ! -x "$BIN" ]; then
    echo "Error: built binary not found at $BIN" >&2
    exit 1
fi

exec "$BIN" "$@"
