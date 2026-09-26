#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder Root — Test Suite & CLI Runner
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
    echo -e "${CYAN}⚡ THUNDER ROOT — TEST SUITE & CLI RUNNER${NC}"
    echo -e "${CYAN}============================================================${NC}"
    echo -e "Rust Version:  $(rustc --version)"
    echo -e "Cargo Version: $(cargo --version)"
    if [ -n "${OPENAI_API_KEY:-}" ]; then
        echo -e "Provider:      ${GREEN}OPENAI_API_KEY set${NC}"
    else
        echo -e "Provider:      ${YELLOW}OPENAI_API_KEY unset${NC} (configure ~/.thunder/auth.json for live runs)"
    fi
    echo ""
}

ok() {
    echo -e "${GREEN}✔ $1${NC}"
}

run_tests() {
    echo -e "${BLUE}▶ Running thunder-root unit & integration tests...${NC}"
    cargo test --quiet
    ok "cargo test (plugin registry, selector & execution tests passed)"
    echo ""
}

run_cli() {
    echo -e "${BLUE}▶ Running thunder-root CLI example...${NC}"
    cargo run --example cli -- "$@"
}

print_help() {
    cat <<EOF
Usage: $0 [command] [args]

Commands:
  (no args)       Run unit and integration tests
  cli [prompt]    Run Thunder-Root CLI example
  test            Run tests only
  help            Show this help

Examples:
  $0
  $0 cli "Implement a custom feature with skills"
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
    cli|run)
        shift
        run_cli "$@"
        ;;
    "")
        print_header
        run_tests
        echo -e "${GREEN}✨ All Thunder-Root test suites completed successfully.${NC}"
        echo -e "To run interactive CLI: ${CYAN}./test.sh cli${NC}"
        ;;
    *)
        run_cli "$@"
        ;;
esac
