#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder Ecosystem — Workspace All-in-One Test Runner
# ────────────────────────────────────────────────────────────

GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
RED='\033[0;31m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if ! command -v cargo &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    elif [ -f "$HOME/.cargo/bin/cargo" ]; then
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        echo -e "${RED}Error: Cargo / Rust is not installed or not in PATH.${NC}"
        exit 1
    fi
fi

echo -e "${CYAN}============================================================${NC}"
echo -e "${CYAN}⚡ THUNDER ECOSYSTEM — FULL WORKSPACE TEST SUITE${NC}"
echo -e "${CYAN}============================================================${NC}"
echo -e "Rust Version:  $(rustc --version)"
echo -e "Cargo Version: $(cargo --version)"
echo ""

# Version consistency first: it is seconds of work, and a release prepared from a
# tree whose crates disagree on the version is one that has to be redone.
echo -e "${BLUE}▶ Checking workspace version consistency...${NC}"
python3 "$SCRIPT_DIR/scripts/check-version.py"
echo ""

echo -e "${BLUE}▶ Running cargo test --workspace (Unified Target)...${NC}"
# --no-fail-fast: cargo stops at the first failing test target by default, so a
# red run would hide every failure in the crates after it (which is exactly how
# the tui platform-test failures above masked whatever came later). Report them all.
cargo test --workspace --no-fail-fast --quiet --features thunder-agent-daemon/testing-mock "$@"

echo -e "${GREEN}✔ All workspace packages passed!${NC}\n"
echo -e "${GREEN}✨ All Thunder crates and test suites completed successfully!${NC}"
