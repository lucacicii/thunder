# Thunder Orchestra

调度器 **B**：组合多只 [`thunder-agent-loop`](../../thunder-agent-loop) 单元（位于 `thunder-agent` monorepo 中）。

[English](README_en.md) | [简体中文](README.md)

A（`thunder-agent-loop`）是一只完整的单 Agent 单元。本仓库是 B：只负责创建、启动、等待、取消多只 A，**不驱动 A 的 turn**。

```
B  ──depends on──►  A (thunder-agent-loop)
A  never imports B
```

## 目录布局

```
~/Documents/GitHub/thunder/thunder-agent-loop                # A (单 Agent 闭环单元)
~/Documents/GitHub/thunder/thunder-agent/                   # thunder-agent monorepo
└── thunder-orchestra/                                      # B (多 Agent 调度器子包)
```

## B 做什么

| B 做 | B 不做 |
|---|---|
| 创建 N 个带独立 id 的 `AgentLoop` | 驱动 A 内部 turn |
| 对每只单元 `start` / `join` / `cancel` | Session UI、Electron、FFI |
| 按 `agent_id` 拆分 `ObservedEvent` | 把 Agent 图塞进 A |
| 落盘 `AgentRunResult.messages` | A 的 loop / prune / tools |
| 可选 `DelegateTool`（A 调 A） | 在 A 里做共享黑板 |

## 拓扑模式与执行机制

| 拓扑 | 定位 | 角色与分工机制 | 适用场景 |
|---|---|---|---|
| `Single` | 单 Agent 自主执行 | 单一全功能 Agent | 绝大多数常规问答、原子命令与独立文件编辑 |
| `Sequential` | 链式串行流水线 | 前序输出注入后续作为输入（Planner ➔ Coder ➔ Reviewer） | 复杂功能规划与渐进式开发 |
| `Parallel` | 多视角并行审查 | **角色视角注入**：每个 Unit 强制绑定专属角色指令与职责边界 | 多维度审查、安全性与性能综合评测 |
| `FanOut` | **真实任务拆解并行** | **结构化任务分解**：`TaskDecomposer` 拆解为独立子任务，分片并发执行 | 大批量任务分块、多模块独立分析 |

### 聚合器（Synthesizer）与文件并发保护

- **结果聚合器（`Synthesizer`）**：在 `Parallel` 或 `FanOut` 结束后，可由可选的聚合节点将多只 Unit 的独立产出归纳提炼，输出统一的决议报告（`synthesis`），避免用户面对多个零散孤立答案。
- **跨 Agent 文件写入互斥锁**：底层由 `TransactionMiddleware` 的进程内全局文件排队锁（`FILE_MUTATION_LOCKS`）保护。多个 Agent 或多工具并发写入同一文件时排队串行化，杜绝竞态覆盖。

## 测试

B 没有 A 那种 example runner，用 `./test.sh`。

默认合同是 **pipeline 交接**：`planner` → `coder`。B 只 `start` / `join`。后一只必须看到前一只的 `final_content`。结果写在 `./runs/<run_id>/<agent_id>.json`。

```bash
cd ~/Documents/GitHub/thunder/thunder-agent/thunder-orchestra

# 默认：cargo test + mock pipeline + 交接断言
./test.sh

# 只跑调度单测
./test.sh test

# Sequential：planner → coder（默认 prompt: "add a health check"）
./test.sh pipeline
./test.sh run "add a health check"

# Parallel：planner + reviewer（只验落盘，不验交接）
./test.sh parallel
```

无 `OPENAI_API_KEY` 时走 Mock，会检查：

- `planner.json` 与 `coder.json` 属于同一个 `run_id`
- 两只都是 `done`，且 `total_turns == 2`
- `coder.final_content` 包含 `planner.final_content`
- coder 的终答不是原始 brief

```bash
# Live LLM（环境和 A 一样；当前进程必须 export key）
export OPENAI_API_KEY=...
export OPENAI_API_BASE=https://api.openai.com/v1
export MODEL=gpt-4o
./test.sh pipeline "add a health check"
```

Live 模式会跳过 Mock 专属的轮次 / 文本检查，但仍要求两只都落盘并产出 `final_content`。

不走脚本时可以直接 cargo：

```bash
cargo test --test scheduler_test
cargo run -- --mock pipeline "add a health check"
cargo run -- parallel "review the repo"
```

### 健康检查

启动真正的 pipeline 之前，先跑一个廉价的、可脚本化的自检：store / scratch 目录可写，LLM（或 mock）可达。

```bash
# 独立运行：打印漂亮的 JSON HealthReport，ok/degraded 退出 0，failed 退出 1
cargo run -- health --mock

# 或走 runner：
./test.sh health
```

报告形态（pretty JSON）：

```json
{
  "status": "ok",
  "checks": [
    { "name": "store_writable",   "passed": true, "detail": "..." },
    { "name": "scratch_writable", "passed": true, "detail": "..." },
    { "name": "llm_reachable",    "passed": true, "detail": "mock client streamed a Completed chunk" }
  ]
}
```

`status` ∈ `ok | degraded | failed`。`health` 与拓扑无关，且永不驱动单元内部的 turn。

## 依赖 A

```toml
thunder-agent-loop = { path = "../../thunder-agent-loop" }
# later:
# thunder-agent-loop = { git = "https://github.com/lucacicii/thunder.git", tag = "v0.1.0" }
```
