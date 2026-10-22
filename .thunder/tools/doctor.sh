#!/usr/bin/env bash
# @thunder
# name: doctor
# description: 检查当前开发环境、Rust工具链、Git状态与Thunder配置健康度
# parameters:
#   type: object
#   properties:
#     verbose:
#       type: boolean
#       description: 是否输出详细诊断日志
# @end

set -euo pipefail

echo "=== Thunder Environment & Health Diagnostics ==="

# 1. Rust Toolchain
if command -v rustc >/dev/null 2>&1; then
    echo "✓ rustc: $(rustc --version)"
else
    echo "✖ rustc: not found in PATH"
fi

if command -v cargo >/dev/null 2>&1; then
    echo "✓ cargo: $(cargo --version)"
else
    echo "✖ cargo: not found in PATH"
fi

# 2. Git Environment
if command -v git >/dev/null 2>&1; then
    echo "✓ git: $(git --version)"
    if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        branch=$(git branch --show-current 2>/dev/null || echo "detached")
        echo "✓ git repository: active (branch: ${branch})"
    else
        echo "ℹ git repository: current directory is not a git worktree"
    fi
else
    echo "✖ git: not found"
fi

# 3. Thunder User Data Root
config_dir="${THUNDER_CONFIG_DIR:-$HOME/.thunder}"
echo "✓ Thunder data root: ${config_dir}"

if [ -f "${config_dir}/models.json" ]; then
    echo "✓ models catalog: found (${config_dir}/models.json)"
else
    echo "ℹ models catalog: missing (${config_dir}/models.json)"
fi

if [ -f "${config_dir}/auth.json" ]; then
    echo "✓ auth credentials: found (${config_dir}/auth.json)"
else
    echo "ℹ auth credentials: missing (${config_dir}/auth.json)"
fi

# 4. Workspace Tools & Prompts
cwd="${THUNDER_CWD:-$(pwd)}"
echo "✓ Workspace root: ${cwd}"

tools_count=0
if [ -d "${cwd}/.thunder/tools" ]; then
    tools_count=$(find "${cwd}/.thunder/tools" -maxdepth 1 -type f ! -name ".*" | wc -l | tr -d ' ')
fi
echo "✓ Local script tools: ${tools_count} in .thunder/tools"

prompts_count=0
if [ -d "${cwd}/.thunder/prompts" ]; then
    prompts_count=$(find "${cwd}/.thunder/prompts" -maxdepth 1 -name "*.md" | wc -l | tr -d ' ')
fi
echo "✓ Reusable prompt templates: ${prompts_count} in .thunder/prompts"

echo "=== Health check completed cleanly ==="
