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
echo -e "${CYAN}⚡ THUNDER ECOSYSTEM — FULL TEST SUITE${NC}"
echo -e "${CYAN}============================================================${NC}"
echo -e "Rust Version:  $(rustc --version)"
echo -e "Cargo Version: $(cargo --version)"
echo ""

echo -e "${BLUE}▶ [1/6] Testing thunder-agent-loop (Core Loop Engine)...${NC}"
(cd thunder-agent-loop && cargo test --quiet)
echo -e "${GREEN}✔ thunder-agent-loop passed!${NC}\n"

echo -e "${BLUE}▶ [2/6] Testing thunder-agent-providers (LLM Adapter Catalog)...${NC}"
(cd thunder-agent-providers && cargo test --quiet)
echo -e "${GREEN}✔ thunder-agent-providers passed!${NC}\n"

echo -e "${BLUE}▶ [3/6] Testing thunder-agent-skills (Skill Parser & Registry)...${NC}"
(cd thunder-agent-skills && cargo test --quiet)
echo -e "${GREEN}✔ thunder-agent-skills passed!${NC}\n"

echo -e "${BLUE}▶ [4/6] Testing thunder-agent-mcp (MCP Client & Tool Bridge)...${NC}"
(cd thunder-agent-mcp && cargo test --quiet)
echo -e "${GREEN}✔ thunder-agent-mcp passed!${NC}\n"

echo -e "${BLUE}▶ [5/6] Testing thunder-agent-root (Microkernel Host & Dynamic Plugins)...${NC}"
(cd thunder-agent-root && cargo test --quiet)
echo -e "${GREEN}✔ thunder-agent-root passed!${NC}\n"

echo -e "${BLUE}▶ [6/6] Testing thunder-agent-core (Conversation, Orchestra, TUI)...${NC}"
(cd thunder-agent-core && cargo test --workspace --quiet)
echo -e "${GREEN}✔ thunder-agent-core passed!${NC}\n"

echo -e "${GREEN}✨ All Thunder crates and test suites completed successfully!${NC}"
