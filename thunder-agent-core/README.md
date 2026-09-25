# ⚡ Thunder Agent Core

> **多 Agent 编排调度、会话持久化与上层终端应用组件库**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-core` 目录收拢了 Thunder 体系中关于多 Agent 编排调度、会话与轮次状态管理、以及终端交互应用的核心功能包。所有子包均已纳入根目录统一 Cargo Workspace 治理。

```text
thunder-agent-core/
├── conversation/               # 【子包】会话与 Session 管理 (thunder-conversation)
├── thunder-orchestra/          # 【子包】多 Agent 编排调度器 (thunder-orchestra)
└── tui/                        # 【子包】Claude Code 风格终端界面 (thunder-tui)
```

---

## 📦 子包概览

- **[`thunder-orchestra`](./thunder-orchestra)**：多 Agent 编排调度器（Scheduler B）。基于 [`thunder-agent-loop`](../../thunder-agent-loop) 构建，支持 `Single`（单 Agent）、`Sequential`（串行流水线）、`Parallel`（多视角并行审查）与 `FanOut`（结构化子任务拆解并发），内置 `Synthesizer` 结果聚合提炼节点与轨迹落盘。
- **[`conversation`](./conversation)**：会话管理子包（`thunder-conversation`）。负责单 Agent 与多 Agent（Sequential 阶段追踪、Parallel 分支合并、Delegate 委托下钻）的会话生命周期、Turn 轮次抽象、Memory/Fs 双模持久化存储（带 `index.json` 极速缓存与原子写入）。
- **[`tui`](./tui)**：交互式终端客户端（`thunder-tui`）。基于 Ratatui 0.29 与 Crossterm 构建，深度集成 `thunder-agent-root`，支持单 Agent 对话、多 Agent 拓扑监视器、实时流式打字机（`TokenDelta`）、思考链折叠（`ReasoningDelta`）、决议聚合报告渲染及全键盘快捷键驱动。

---

## 🛠️ 自动化测试

```bash
# 测试 thunder-orchestra
cargo test -p thunder-orchestra

# 测试 thunder-conversation
cargo test -p thunder-conversation

# 测试 thunder-tui
cargo test -p thunder-tui
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](LICENSE) 开源许可证。
