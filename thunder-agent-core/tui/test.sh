#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder TUI — Test Suite & Runner
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
    else
        echo -e "${RED}Error: Cargo / Rust is not installed or not in PATH.${NC}"
        exit 1
    fi
fi

print_header() {
    echo -e "${CYAN}============================================================${NC}"
    echo -e "${CYAN}⚡ THUNDER TUI — TEST SUITE & RUNNER${NC}"
    echo -e "${CYAN}============================================================${NC}"
    echo -e "Rust Version:  $(rustc --version)"
    echo -e "Cargo Version: $(cargo --version)"
    if [ -n "${OPENAI_API_KEY:-}" ]; then
        echo -e "LLM Mode:      ${GREEN}live${NC} (OPENAI_API_KEY set)"
    else
        echo -e "LLM Mode:      ${YELLOW}mock${NC} (OPENAI_API_KEY unset)"
    fi
    echo ""
}

ok() {
    echo -e "${GREEN}✔ $1${NC}"
}

run_tests() {
    echo -e "${BLUE}▶ Running TUI unit & integration tests...${NC}"
    cargo test --quiet
    ok "cargo test (all TUI state & event tests passed)"
    echo ""
}

run_tui() {
    echo -e "${BLUE}▶ Launching Thunder TUI...${NC}"
    cargo run --bin thunder-tui -- "$@"
}

run_mock() {
    echo -e "${BLUE}▶ Launching Thunder TUI (Mock Mode)...${NC}"
    cargo run --bin thunder-tui -- --mock "$@"
}

print_help() {
    cat <<EOF
Usage: $0 [command] [args]

Commands:
  (no args)       Run unit and integration tests
  run [args]      Launch Thunder TUI (cargo run --bin thunder-tui -- ...)
  mock [args]     Launch Thunder TUI in Mock mode (--mock)
  test            Run unit tests only
  help            Show this help

Examples:
  $0 run
  $0 mock
  $0 run --session sess_1787560000000
EOF
}

cmd="${1:-}"
case "$cmd" in
    help|--help|-h)
        print_help
        ;;
    test|tests)
        print_header
        run_tests
        ;;
    run|tui|start)
        shift
        run_tui "$@"
        ;;
    mock)
        shift
        run_mock "$@"
        ;;
    "")
        print_header
        run_tests
        echo -e "${GREEN}✨ All TUI test suites completed successfully.${NC}"
        echo -e "To launch interactive TUI: ${CYAN}./test.sh run${NC} or ${CYAN}./run.sh${NC}"
        ;;
    *)
        run_tui "$@"
        ;;
esac
