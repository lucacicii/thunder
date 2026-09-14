# ⚡ Thunder Agent Core Monorepo

> **Multi-Agent Orchestration, Conversation & Applications**

[English](README_en.md) | [简体中文](README.md)

---

## 📦 Monorepo 架构与子包

`thunder-agent-core` 承载多 Agent 编排调度、会话存储及上层终端应用模块。

```
thunder-agent-core/
├── Cargo.toml                  # Workspace 根配置文件
├── conversation/               # 【子包】会话与 Session 管理 (thunder-conversation)
├── thunder-orchestra/          # 【子包】多 Agent 调度器 (thunder-orchestra)
├── tui/                        # 【子包】交互式终端界面 (thunder-tui)
└── ...                         # 后续可扩展更多业务子包
```

### 子包索引

- **[`tui`](./tui)**: 交互式终端客户端（`thunder-tui`）。基于 Ratatui 0.29 与 Crossterm 构建，深度集成 `thunder-agent-root`，支持单 Agent 交互对话、多 Agent 编排监视器、实时流式打字机效果（`TokenDelta`）、思考链折叠（`ReasoningDelta`）及全键盘快捷键驱动。
- **[`conversation`](./conversation)**: 会话管理子包（`thunder-conversation`）。负责单 Agent 与多 Agent（Sequential 流水线阶段、Parallel 分支合并、Delegate 委托下钻）的会话生命周期、Turn 轮次抽象、Memory/Fs 双模持久化存储（带 `index.json` 缓存与原子写入）与导出。
- **[`thunder-orchestra`](./thunder-orchestra)**: 多 Agent 调度器（Scheduler B）。基于 [`thunder-agent-loop`](../thunder-agent-loop) 单元构建，支持流水线串行交接（Pipeline Sequential）、并行执行（Parallel）、委托调用（`DelegateTool`）以及执行轨迹落盘（`RunStore`）。

---

## 🛠️ 快速开始

### 运行工作区测试

```bash
# 在 monorepo 根目录下运行 workspace 测试
cargo test --workspace

# 运行 thunder-orchestra 自动化测试套件
cd thunder-orchestra
./test.sh
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](LICENSE) 许可证。
