#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder — Root Launch Script for Interactive TUI
#
# The TUI treats its current directory as the workspace (and the
# security jail root), so this script MUST NOT change the cwd of the
# program it launches: run it from the project you want to work in.
# The build step cd's into the cargo workspace inside a subshell only.
# ────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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

# Build from the cargo workspace root (subshell: cwd change does not leak).
( cd "$SCRIPT_DIR" && cargo build -p thunder-tui --bin thunder-tui )

TARGET_DIR="${CARGO_TARGET_DIR:-$SCRIPT_DIR/target}"
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$SCRIPT_DIR/$TARGET_DIR" ;;
esac
BIN="$TARGET_DIR/debug/thunder-tui"

if [ ! -x "$BIN" ]; then
    echo "Error: built binary not found at $BIN"
    exit 1
fi

# Run in the caller's cwd (preserved) so it becomes the workspace root.
exec "$BIN" "$@"
