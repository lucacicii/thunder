#!/usr/bin/env bash
set -eo pipefail

# ────────────────────────────────────────────────────────────
# Thunder Agent Loop — Fast Bash Test & Verification Script
# ────────────────────────────────────────────────────────────

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
RED='\033[0;31m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Ensure Cargo/Rust is in PATH
if ! command -v cargo &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        source "$HOME/.cargo/env"
    else
        echo -e "${RED}Error: Cargo / Rust is not installed or not in PATH.${NC}"
        echo "Please install Rust via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        exit 1
    fi
fi

print_header() {
    echo -e "${CYAN}============================================================${NC}"
    echo -e "${CYAN}⚡ THUNDER AGENT LOOP — TEST SUITE & RUNNER${NC}"
    echo -e "${CYAN}============================================================${NC}"
    echo -e "Rust Version: $(rustc --version)"
    echo -e "Cargo Version: $(cargo --version)"
    echo ""
}

run_unit_tests() {
    echo -e "${BLUE}▶ [1/3] Running Unit & Integration Tests (Debug + Release)...${NC}"
    cargo test --all-targets --quiet
    echo -e "${GREEN}✔ All tests passed successfully!${NC}\n"
}

run_benchmarks() {
    echo -e "${BLUE}▶ [2/3] Running Performance & Concurrency Benchmark...${NC}"
    cargo run --release --example benchmark --quiet
    echo -e "${GREEN}✔ Benchmark completed!${NC}\n"
}

run_cli_example() {
    local PROMPT="${1:-Check system status and disk space using bash}"
    echo -e "${BLUE}▶ [3/3] Running Agent Loop CLI (Live Event Streaming)...${NC}"
    cargo run --release --example cli -- "$PROMPT"
    echo -e "${GREEN}✔ CLI execution finished!${NC}\n"
}

case "$1" in
    test|tests)
        print_header
        run_unit_tests
        ;;
    bench|benchmark)
        print_header
        run_benchmarks
        ;;
    run|cli)
        shift
        print_header
        run_cli_example "$@"
        ;;
    help|--help|-h)
        echo "Usage: $0 [command] [args]"
        echo ""
        echo "Commands:"
        echo "  (no args)       Run full test suite (Tests + Benchmark + CLI Demo)"
        echo "  test            Run unit and integration test suite"
        echo "  bench           Run 10,000 concurrency & token throughput benchmark"
        echo "  run [prompt]    Run live CLI demo with streaming event logs"
        echo ""
        echo "Environment Variables (for live LLM):"
        echo "  OPENAI_API_KEY   Your API key (omit to use built-in Mock LLM)"
        echo "  OPENAI_API_BASE  API endpoint (defaults to https://api.openai.com/v1)"
        echo "  MODEL            Model ID (defaults to gpt-4o)"
        ;;
    *)
        print_header
        run_unit_tests
        run_benchmarks
        run_cli_example "$@"
        echo -e "${GREEN}🎉 All checks passed! Thunder Agent Loop is fully verified.${NC}"
        ;;
esac
