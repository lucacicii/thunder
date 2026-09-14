#!/usr/bin/env bash
set -euo pipefail

# ────────────────────────────────────────────────────────────
# Thunder Orchestra (B) — Test Suite & Runner
# Default case: pipeline planner → coder handoff + persist
# ────────────────────────────────────────────────────────────

GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
RED='\033[0;31m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

DEFAULT_PIPELINE_PROMPT="add a health check"
DEFAULT_PARALLEL_PROMPT="review the repo"

if ! command -v cargo &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    else
        echo -e "${RED}Error: Cargo / Rust is not installed or not in PATH.${NC}"
        echo "Please install Rust via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        exit 1
    fi
fi

print_header() {
    echo -e "${CYAN}============================================================${NC}"
    echo -e "${CYAN}⚡ THUNDER ORCHESTRA — TEST SUITE & RUNNER${NC}"
    echo -e "${CYAN}============================================================${NC}"
    echo -e "Rust Version: $(rustc --version)"
    echo -e "Cargo Version: $(cargo --version)"
    if [ -n "${OPENAI_API_KEY:-}" ]; then
        echo -e "LLM Mode:     ${GREEN}live${NC} (OPENAI_API_KEY set)"
    else
        echo -e "LLM Mode:     ${YELLOW}mock${NC} (OPENAI_API_KEY unset)"
    fi
    echo ""
}

fail() {
    echo -e "${RED}✘ $1${NC}"
    exit 1
}

ok() {
    echo -e "${GREEN}✔ $1${NC}"
}

json_get() {
    local file="$1"
    local key="$2"
    python3 - "$file" "$key" <<'PY'
import json, sys
path, key = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as fh:
    data = json.load(fh)
value = data
for part in key.split("."):
    if isinstance(value, dict) and part in value:
        value = value[part]
    else:
        sys.exit(1)
if value is None:
    print("")
elif isinstance(value, (dict, list)):
    print(json.dumps(value, ensure_ascii=False))
else:
    print(value)
PY
}

assert_eq() {
    local label="$1"
    local actual="$2"
    local expected="$3"
    if [ "$actual" != "$expected" ]; then
        fail "$label: expected '$expected', got '$actual'"
    fi
    ok "$label = $expected"
}

assert_contains() {
    local label="$1"
    local haystack="$2"
    local needle="$3"
    if [[ "$haystack" != *"$needle"* ]]; then
        fail "$label: missing '$needle'"
    fi
    ok "$label contains expected fragment"
}

assert_not_eq() {
    local label="$1"
    local actual="$2"
    local forbidden="$3"
    if [ "$actual" = "$forbidden" ]; then
        fail "$label: must not equal '$forbidden'"
    fi
    ok "$label is not the raw brief"
}

latest_run_dir() {
    local marker="$1"
    python3 - "$SCRIPT_DIR/runs" "$marker" <<'PY'
import pathlib, sys
root = pathlib.Path(sys.argv[1])
marker = sys.argv[2]
if not root.is_dir():
    sys.exit(1)
candidates = []
for path in root.iterdir():
    if not path.is_dir() or not path.name.startswith("run_"):
        continue
    if not (path / marker).is_file():
        continue
    candidates.append(path)
if not candidates:
    sys.exit(1)
print(max(candidates, key=lambda p: p.stat().st_mtime))
PY
}

run_unit_tests() {
    echo -e "${BLUE}▶ Running scheduler unit tests...${NC}"
    cargo test --test scheduler_test --quiet
    ok "cargo test --test scheduler_test"
    echo ""
}

run_cli() {
    local topology="$1"
    shift
    local prompt="${*:-}"
    if [ -z "$prompt" ]; then
        if [ "$topology" = "pipeline" ]; then
            prompt="$DEFAULT_PIPELINE_PROMPT"
        else
            prompt="$DEFAULT_PARALLEL_PROMPT"
        fi
    fi

    echo -e "${BLUE}▶ Running orchestra CLI (${topology})...${NC}"
    echo -e "   Brief: ${prompt}"
    cargo run --release --quiet -- "$topology" "$prompt"
    echo ""
}

run_health_check() {
    echo -e "${BLUE}▶ Running health check (mock)...${NC}"
    cargo run --quiet -- health --mock
    assert_eq "health exit code" "$?" "0"
    echo ""
}

# Default contract: Sequential planner → coder.
# Mock turn 0 calls bash; turn 1 stops with "[role] done. Prior task was: {last_user}".
# Sequential injects planner.final_content into coder's incoming brief.
assert_pipeline_handoff() {
    local prompt="$1"
    echo -e "${BLUE}▶ Asserting default case: pipeline planner → coder...${NC}"

    local run_dir
    run_dir="$(latest_run_dir "coder.json")" \
        || fail "no ./runs/<run_id>/coder.json — pipeline did not persist"

    local planner_json="$run_dir/planner.json"
    local coder_json="$run_dir/coder.json"
    [ -f "$planner_json" ] || fail "missing $planner_json"
    [ -f "$coder_json" ] || fail "missing $coder_json"
    ok "persisted $(basename "$run_dir")/{planner,coder}.json"

    local planner_id coder_id planner_role coder_role
    local planner_reason coder_reason planner_turns coder_turns
    local planner_final coder_final planner_run coder_run
    planner_id="$(json_get "$planner_json" agent_id)"
    coder_id="$(json_get "$coder_json" agent_id)"
    planner_role="$(json_get "$planner_json" role)"
    coder_role="$(json_get "$coder_json" role)"
    planner_reason="$(json_get "$planner_json" finish_reason)"
    coder_reason="$(json_get "$coder_json" finish_reason)"
    planner_turns="$(json_get "$planner_json" total_turns)"
    coder_turns="$(json_get "$coder_json" total_turns)"
    planner_final="$(json_get "$planner_json" final_content)"
    coder_final="$(json_get "$coder_json" final_content)"
    planner_run="$(json_get "$planner_json" run_id)"
    coder_run="$(json_get "$coder_json" run_id)"

    assert_eq "planner.agent_id" "$planner_id" "planner"
    assert_eq "coder.agent_id" "$coder_id" "coder"
    assert_eq "planner.role" "$planner_role" "planner"
    assert_eq "coder.role" "$coder_role" "coder"
    assert_eq "planner.run_id == coder.run_id" "$planner_run" "$coder_run"
    assert_eq "planner.finish_reason" "$planner_reason" "done"
    assert_eq "coder.finish_reason" "$coder_reason" "done"

    if [ -z "${OPENAI_API_KEY:-}" ]; then
        assert_eq "planner.total_turns" "$planner_turns" "2"
        assert_eq "coder.total_turns" "$coder_turns" "2"
        assert_contains "planner.final_content" "$planner_final" "[planner]"
        assert_contains "coder.final_content" "$coder_final" "[coder]"
        assert_contains "coder received planner final_content" "$coder_final" "$planner_final"
        assert_not_eq "coder.final_content" "$coder_final" "$prompt"
    else
        echo -e "${YELLOW}! live mode: skip mock-specific turn/handoff text checks${NC}"
        [ -n "$planner_final" ] || fail "planner.final_content is empty"
        [ -n "$coder_final" ] || fail "coder.final_content is empty"
        ok "both units produced final_content"
    fi

    echo ""
    echo -e "${GREEN}🎉 Default pipeline case passed.${NC}"
    echo -e "   run_id:  $planner_run"
    echo -e "   store:   $run_dir"
}

assert_parallel_persist() {
    echo -e "${BLUE}▶ Asserting parallel persist: planner + reviewer...${NC}"
    local run_dir
    run_dir="$(latest_run_dir "reviewer.json")" \
        || fail "no ./runs/<run_id>/reviewer.json — parallel did not persist"
    [ -f "$run_dir/planner.json" ] || fail "missing $run_dir/planner.json"
    [ -f "$run_dir/reviewer.json" ] || fail "missing $run_dir/reviewer.json"
    ok "persisted $(basename "$run_dir")/{planner,reviewer}.json"
    echo ""
}

print_help() {
    cat <<EOF
Usage: $0 [command] [prompt]

Commands:
  (no args)              Default case: cargo test + mock pipeline + handoff asserts
  test                   Run scheduler unit tests only
  pipeline [prompt]      Run Sequential planner → coder (default: "${DEFAULT_PIPELINE_PROMPT}")
  parallel [prompt]      Run Parallel planner + reviewer (default: "${DEFAULT_PARALLEL_PROMPT}")
  run [prompt]           Alias of pipeline (the default B contract)
  health|--health        Run the cheap self-check (writable dirs + mock LLM)
  help                   Show this help

Environment (same as A):
  OPENAI_API_KEY         Set to use live LLM; omit / --mock path if unset
  OPENAI_API_BASE        Default https://api.openai.com/v1
  MODEL                  Default gpt-4o

Default contract (mock):
  planner → coder, each 2 turns / 1 bash tool, coder.final_content
  contains planner.final_content, results under ./runs/<run_id>/*.json
EOF
}

cmd="${1:-}"
case "$cmd" in
    help|--help|-h)
        print_help
        ;;
    test|tests)
        print_header
        run_unit_tests
        ;;
    parallel|--parallel)
        shift
        print_header
        run_cli parallel "$@"
        assert_parallel_persist
        ;;
    pipeline|sequential|--pipeline|run|cli)
        shift
        print_header
        prompt="${*:-$DEFAULT_PIPELINE_PROMPT}"
        run_cli pipeline "$prompt"
        assert_pipeline_handoff "$prompt"
        ;;
    health|--health)
        print_header
        run_health_check
        ;;
    "")
        print_header
        run_unit_tests
        run_cli pipeline "$DEFAULT_PIPELINE_PROMPT"
        assert_pipeline_handoff "$DEFAULT_PIPELINE_PROMPT"
        echo -e "${GREEN}🎉 All B checks passed.${NC}"
        ;;
    *)
        print_header
        prompt="$*"
        run_cli pipeline "$prompt"
        assert_pipeline_handoff "$prompt"
        ;;
esac
