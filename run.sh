#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder — Root Launch Script for Interactive TUI
#
# The TUI treats its current directory as the workspace (and the
# security jail root), so this script MUST NOT change the cwd of the
# program it launches: run it from the project you want to work in.
# The build step cd's into the cargo workspace inside a subshell only.
#
# Build profile: release by default. An interactive TUI is judged by how it
# feels, and the debug profile is roughly an order of magnitude slower in the
# render path, which is what makes a long session feel like a space heater.
#   THUNDER_TUI_PROFILE=tui-dev ./run.sh   # optimized, but links in seconds
#   THUNDER_TUI_PROFILE=dev     ./run.sh   # unoptimized, for a debugger
# ────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PROFILE="${THUNDER_TUI_PROFILE:-release}"
case "$PROFILE" in
    release|tui-dev) ;;
    dev) ;;
    *)
        echo "Error: THUNDER_TUI_PROFILE must be release, tui-dev or dev (got '$PROFILE')." >&2
        exit 2
        ;;
esac

# Cargo's directory for the `dev` profile is `debug`.
case "$PROFILE" in
    dev) PROFILE_DIR="debug" ;;
    *) PROFILE_DIR="$PROFILE" ;;
esac

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
( cd "$SCRIPT_DIR" && cargo build --profile "$PROFILE" -p thunder-tui --bin thunder-tui )

TARGET_DIR="${CARGO_TARGET_DIR:-$SCRIPT_DIR/target}"
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$SCRIPT_DIR/$TARGET_DIR" ;;
esac
BIN="$TARGET_DIR/$PROFILE_DIR/thunder-tui"

if [ ! -x "$BIN" ]; then
    echo "Error: built binary not found at $BIN"
    exit 1
fi

# Run in the caller's cwd (preserved) so it becomes the workspace root.
exec "$BIN" "$@"
