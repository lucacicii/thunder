#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PROFILE="${THUNDER_TUI_PROFILE:-release}"
case "$PROFILE" in
    release|tui-dev|dev) ;;
    *)
        echo "Error: THUNDER_TUI_PROFILE must be release, tui-dev or dev (got '$PROFILE')." >&2
        exit 2
        ;;
esac

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

( cd "$SCRIPT_DIR" && cargo build --profile "$PROFILE" -p thunder-std -p thunder-tui )

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

exec "$BIN" "$@"
