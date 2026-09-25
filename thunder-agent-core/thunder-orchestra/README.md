# Thunder Orchestra

> **多 Agent 编排调度器（Scheduler B）**

[English](README_en.md) | [简体中文](README.md)

Thunder Orchestra 是负责组合多只 [`thunder-agent-loop`](../../thunder-agent-loop) 单元的高级调度器。

在 Thunder 的 **A/B 架构契约** 中：
- **A（`thunder-agent-loop`）**：是一只完整的单 Agent 闭环单元，负责多轮推理、工具执行与上下文维护。
- **B（`thunder-orchestra`）**：是高层编排调度器，只负责创建、启动、监控、合并、取消多只 A，**绝不驱动 A 内部的 turn**。

```text
B (thunder-orchestra)  ──依赖于──►  A (thunder-agent-loop)
A  绝不依赖 B
```

---

## 目录布局

```text
thunder/
├── thunder-agent-loop/                               # A (单 Agent 闭环单元)
└── thunder-agent-core/
    └── thunder-orchestra/                            # B (多 Agent 调度器)
        ├── src/
        │   ├── config.rs                             # OrchestraConfig、Topology、UnitSpec
        │   ├── decomposer.rs                         # TaskDecomposer、SubTask 结构化拆解
        │   ├── delegate.rs                           # DelegateTool (A 调 A 工具桥)
        │   ├── router.rs                             # IntentRouter 确定性拓扑路由
        │   ├── scheduler.rs                          # Scheduler (Sequential / Parallel / FanOut)
        │   ├── store.rs                              # RunStore 轨迹落盘
        │   └── synthesizer.rs                        # Synthesizer 结果聚合与决议提炼
        └── tests/
```

---

## B 做什么与不做什么

| B 负责 | B 绝不插手 |
|---|---|
| 创建并管理 N 个带独立 `agent_id` 的 `AgentLoop` | 驱动 A 内部的 turn 细节 |
| 对各单元执行 `start` / `join` / `cancel` | Session UI、Electron、FFI 视图渲染 |
| 按 `agent_id` 解复用 `ObservedEvent` 事件流 | 把全局 Agent 图强塞进 A 内部 |
| 将各单元产出的 `AgentRunResult` 持久化落盘 | 破坏 A 的 loop / prune / tools 封装 |
| 提供可选的 `DelegateTool`（让 A 能够下钻委托子 A） | 在 A 内部维护隐式共享黑板 |

---

## 拓扑模式与执行机制

| 拓扑模式 | 定位 | 角色与分工机制 | 适用场景 |
|---|---|---|---|
| **`Single`** | 单 Agent 自主执行 | 单一全功能 Agent 自主思考与执行 | 绝大多数常规问答、原子命令与独立文件编辑 |
| **`Sequential`** | 链式串行流水线 | 前序输出注入后续作为输入（Planner ➔ Coder ➔ Reviewer） | 复杂功能规划与渐进式开发流程 |
| **`Parallel`** | 多视角并行审查 | **角色视角注入**：每个 Unit 强制绑定专属系统提示词与职责边界 | 多维度审查、安全性与性能综合评测 |
| **`FanOut`** | **真实任务拆解并行** | **结构化任务分解**：`TaskDecomposer` 将庞大指令切片为独立子任务分片并发执行 | 大批量任务分块、多模块独立分析 |

### 聚合器（Synthesizer）与文件并发保护

- **结果聚合器（`Synthesizer`）**：在 `Parallel` 或 `FanOut` 结束后，由可选的聚合节点将多只 Unit 的独立产出归纳提炼，输出统一的决议报告（`synthesis`），避免用户面对多个零散孤立答案。
- **跨 Agent 文件写入互斥排队锁**：底层由 `TransactionMiddleware` 的进程内全局文件排队锁（`FILE_MUTATION_LOCKS`）保护。多个 Agent 或多工具并发写入同一文件时排队串行化，杜绝竞态覆盖。

---

## 测试与运行

默认验证合同包含：
- **Pipeline 流水线交接**：`planner` ➔ `coder`。后一只必须看到前一只的 `final_content`。
- **Parallel 并行审查与角色注入**：各单元拥有独立的 `role` 提示词。
- **FanOut 任务拆解与并发执行**：按列表或角色分片，各单元并发处理专属子任务。
- **Synthesizer 决议聚合**：各单元完成后产出结构化聚合总结。

结果统一持久化在 `./runs/<run_id>/<agent_id>.json`。

### 运行脚本

```bash
cd thunder-agent-core/thunder-orchestra

# 运行全套单测与集成测试
cargo test --test scheduler_test
cargo test --test router_test

# 启动模拟 (Mock) 模式进行流水线验证
cargo run -- --mock pipeline "add a health check"

# 启动并行审查
cargo run -- --mock parallel "review the security and performance of src/lib.rs"

# 启动子任务拆解与并发 Fan-Out
cargo run -- --mock fanout "1. Refactor networking 2. Optimize parser 3. Update tests"

# 运行健康检查自检（检查 store/scratch 可写性与 LLM 连通性）
cargo run -- health --mock
```

### 健康检查输出形态

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

---

## 依赖关系

```toml
[dependencies]
thunder-agent-loop = { path = "../../thunder-agent-loop" }
```
