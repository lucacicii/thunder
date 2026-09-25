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
| **`FanOut`** | **子任务拆解并行** | **启发式任务分解**：`HeuristicDecomposer` 识别编号/项目符号列表切片并发执行；自然语言 prompt 则按角色视角分片 | 批量列表任务分块、多模块独立分析 |

### 聚合器（Synthesizer）与文件并发保护

- **结果聚合器（`Synthesizer`）**：在 `Parallel` 或 `FanOut` 结束后，由可选的聚合节点将多只 Unit 的独立产出归纳提炼，输出统一的决议报告（`synthesis`），避免用户面对多个零散孤立答案。**诚实降级**：仅当真实模式下配置了聚合 Unit 且成功解析 LLM client 时才执行 LLM 综合；否则原样并列各 Unit 产出并标注原因，绝不伪造「已综合」结论。
- **跨 Agent 文件写入互斥排队锁**：底层由 `TransactionMiddleware` 的**进程内**全局文件排队锁（`FILE_MUTATION_LOCKS`）保护。多个 Agent 或多任务并发写入同一文件时排队串行化，不同文件完全并行；锁表条目在无竞争写入后 best-effort 释放，长跑进程不会无限膨胀。注意：该锁不跨进程（TUI 与 daemon 同时写同一文件时无互斥保证）。

### 真实模式 Client 注入（关键）

B 是纯调度器，**自身不解析传输层**。真实模式（`use_mock = false`）下必须由组合根（TUI / daemon / CLI）注入 `ClientFactory`：

```rust
use std::sync::Arc;
use thunder_agent_loop::AgentConfig;
use thunder_orchestra::{ClientFactory, OrchestraConfig, Topology, UnitSpec};

let factory: ClientFactory = Arc::new(|cfg: &AgentConfig| {
    my_registry
        .resolve(&cfg.model)
        .and_then(|spec| my_client_for(spec, cfg.request_timeout_ms).ok())
});

let orchestra = OrchestraConfig::new(Topology::Parallel)
    .with_client_factory(factory)          // 真实模式必需
    .with_unit(UnitSpec::new("planner", "planner", base.clone()));
```

**Fail-Fast 语义**：
- 真实模式下未注入 factory → `dispatch` 在任何 Unit 启动前立即报错（而非运行中途每个 Unit 死在 `UnconfiguredLLMClient` 上）。
- factory 对某个 Unit 返回 `None` → 报错并指明 Unit 与模型名。
- 聚合器无法解析 client 时诚实降级（见上节），不伪造综合报告。
- `health` 真实模式探针同样走 factory。

TUI 已在内部从 `ProviderRegistry` 自动构造 factory（与 `ThunderRoot` 同一 `client_for` 路径），并为 planner/coder/reviewer/synthesizer 注入差异化 persona 系统提示词；CLI 则在 `--mock` 缺席时从默认 registry 解析，解析失败时以退出码 `2` 提示配置缺失。

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
# 真实模式（非 mock 分支）注入链路测试
cargo test --test real_mode_test

# 启动模拟 (Mock) 模式进行流水线验证
cargo run -- --mock pipeline "add a health check"

# 启动并行审查
cargo run -- --mock parallel "review the security and performance of src/lib.rs"

# 启动子任务拆解与并发 Fan-Out
cargo run -- --mock fanout "1. Refactor networking 2. Optimize parser 3. Update tests"

# 真实模式（读取 ~/.thunder/models.json + auth.json；未配置时退出码 2）
MODEL=deepseek-chat cargo run -- parallel "review src/lib.rs"

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
# 仅 `thunder-orchestra` 二进制（组合根）用于构建真实模式 factory；库本身不依赖传输层
thunder-agent-providers = { path = "../../thunder-agent-providers" }
```
