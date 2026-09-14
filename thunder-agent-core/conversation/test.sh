#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder Conversation — Test Suite & Runner
# ────────────────────────────────────────────────────────────

GREEN='\033[0;32m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
RED='\033[0;31m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if ! command -v cargo &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    else
        echo -e "${RED}Error: Cargo / Rust is not installed or not in PATH.${NC}"
        exit 1
    fi
fi

print_header() {
    echo -e "${CYAN}============================================================${NC}"
    echo -e "${CYAN}⚡ THUNDER CONVERSATION — TEST SUITE${NC}"
    echo -e "${CYAN}============================================================${NC}"
    echo -e "Rust Version:  $(rustc --version)"
    echo -e "Cargo Version: $(cargo --version)"
    echo ""
}

ok() {
    echo -e "${GREEN}✔ $1${NC}"
}

run_tests() {
    echo -e "${BLUE}▶ Running conversation tests (CRUD, FsStore, Bridge, Orchestration)...${NC}"
    cargo test --quiet
    ok "cargo test (all unit & integration tests passed)"
    echo ""
}

print_header
run_tests
echo -e "${GREEN}✨ All conversation test suites completed successfully.${NC}"
