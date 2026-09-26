# 架构设计：A 是原子单元，B 是编排调度器

[English](ARCHITECTURE.md) | [简体中文](ARCHITECTURE_zh.md)

本 Crate（`thunder-agent-loop`）是 **Agent 内核**：一个完整、可独立运行的单 Agent 闭环单元。宿主应用（如 `thunder-agent-root`）可以依赖内核并驱动它执行任务。**内核绝不依赖宿主**。

```text
A  = 单个 AgentLoop，一次跑一个任务，拥有完整的内部 turn 闭环
B  = 可选的调度器 / 多 Agent 图拓扑（外部下游 crate）
```

## 职责分层

| 维度 | A（本 Crate：闭环单元） | B（下游应用：调度器） |
|---|---|---|
| **角色定位** | 完整的单 Agent 闭环 | 多 Agent 调度器 / 工作流拓扑图 |
| **生命周期** | 创建 ➔ 注册工具 ➔ `start`/`run` ➔ 流式事件 ➔ join/cancel | 创建 N 个 A 单元，任务路由、重试与业务落盘 |
| **对话范围** | 单任务内部的多轮推理（Loop 核心职责） | 跨 Agent / 跨会话的交接路由与存储 |
| **独立性** | 必需。A 脱离 B 可独立运行全生命周期 | 可选。B 依赖 A 组装业务流 |
| **组合方式** | B 调用 `AgentLoop::start` / `join` / `cancel` | B 绝不可接管或拆散 A 内部的 turn |

A 拥有闭环的 `run()` / `start()` 机制：向 LLM 订阅流式 Token、并发执行工具、裁剪上下文日志、触发熔断守卫并以 `LoopComplete` 结束。这是**单元完整性**，绝非“抢了 B 的工作”。

B **严禁**驱动 A 内部的 turn。不要把 A 拆成外部步进器（把 `step` + `execute_tools` 当作主要集成表面）。`step` 不是对外的集成契约。

B 可以在 `join()` 之后持久化 `AgentRunResult.messages`。那是 B 的业务存储，绝不意味着 A 缺失会话层。

---

## 单元调用契约

```rust
let mut agent = AgentLoop::new(config).with_id("researcher");
agent.register_tool(Arc::new(BashTool::default()));

let handle = agent.start("Investigate the repo", None)?;
// handle.agent_id() == "researcher"
// handle.events() 是可靠的单任务流式通道（自带背压，不丢事件）
// handle.status() 实时可查
// handle.cancel() 仅停止本任务

let result = handle.join().await?;
```

- `run()` 等价于 `start` + 消费事件 + `join`。CLI 与测试用 `run()`。
- 调度器使用 `start`，从而能够并行驱动多个 A 实例。

### 核心规则

1. **单个 `AgentLoop` 同一时刻只处理一个任务**：忙碌时再次调用 `start`/`run` 会直接返回 `AgentError::AlreadyRunning`。需要并行时只需 `new` 另一个实例。
2. **事件均附带 `agent_id` 标签**：多 Agent 进程可通过该标签轻松解复用。
3. **Scratchpad 临时目录按 `agent_id` 物理隔离**：除非显式设置 `auto_cleanup`，否则 A 不会自动删除。
4. **跨 Agent 文件写入互斥排队（`FILE_MUTATION_LOCKS`）**：在 `TransactionMiddleware` 中内置进程内全局规范化路径排队锁，多 A 或多工具并发写入同一文件自动排队串行化，杜绝竞态覆盖。
5. **A 不安装全局 `tracing` 订阅者**：由宿主（CLI、Daemon 或 B）统筹安装。
6. **A 完全不感知 B**：A 内部没有 Agent 图、没有路由器、没有黑板模式。
7. **嵌套多 Agent（A 调用 A）在 B 中体现为工具**（通过 `DelegateTool` 包裹另一个 `AgentLoop`），严禁在 A 内部衍生运行时拓扑图。

---

## 留在 A 内部的能力

- 完整的闭环自主循环（`run` / `start`）
- `LLMClientTrait` 传输抽象契约（生产环境由 `thunder-pi-bridge` 实现）
- 工具注册表、并发工具派发、超时熔断与 UTF-8 截断
- 内存上下文裁剪，支持解耦工具输出驱逐（`tool_eviction_threshold_tokens`）
- 全局路径排队锁（`FILE_MUTATION_LOCKS`）
- 循环级防死循环熔断器（默认 50 轮上限）
- 基于 `CancellationToken` 的任务取消与基于 `PauseGate` 的协作式暂停
- 核心内置工具部件（`BashTool`、`ReadFileTool`、`WriteFileTool`，按需注册）

---

## 绝不进入 A 的逻辑

- 会话列表、Session UI、多窗口渲染
- Electron / FFI / Node.js 运行时绑定
- 多 Agent 拓扑关系、路由表、黑板模式
- 宿主全局 Tracing 订阅者与 Tokio Runtime 所有权
- 业务策略：API Key 物理存储、权限规则持久化

---

## 依赖流向

```text
Host (thunder-agent-root)  ──依赖于──►  Kernel (thunder-agent-loop)
A  绝不引用任何 B
```

B 通过 Path、Git 标签或版本依赖 A。A 的持续集成（CI）绝不构建 B。
