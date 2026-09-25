# Thunder

> **极轻量、高性能、模块化的 Rust AI Agent 工作区与运行时架构**

[English](README_en.md) | [简体中文](README.md)

Thunder 是基于 Rust 构建的现代化 AI Agent 体系。整个生态采用单 Git Monorepo 与统一 Cargo Workspace 治理，提供从极速单 Agent 闭环底座、统一模型传输、微内核动态插件编排、到多智能体分工协同（流水线、并行评审、子任务拆解与结果聚合）的完整解决方案。

仓库托管在 [lucacicii/thunder](https://github.com/lucacicii/thunder)。关于工作区布局、架构依赖及开发约定，请参阅 **[WORKSPACE.md](WORKSPACE.md)**。

---

## ⚡ 核心架构支柱

1. **A/B 架构契约（Unit vs Scheduler）**：
   - **Agent A（`thunder-agent-loop`）**：单一任务的原子闭环执行体，负责驱动完整的多轮推理、流式 Token 接收、并发工具调度与上下文裁剪。绝不耦合任何多智能体图或黑板逻辑。
   - **Agent B（`thunder-orchestra`）**：高层调度器，负责编排 N 个独立的 A 单元，支持链式流水线（`Sequential`）、角色绑定并行评审（`Parallel`）及结构化子任务分片（`FanOut`），并由 `Synthesizer` 节点聚合成统一决策报告。
2. **pi-bridge 单一传输底座**：
   - 彻底废弃维护成本高昂的自定义 Rust 模型方言适配器。
   - 统一由 `thunder-pi-bridge` 驱动通过 esbuild 预打包的 `@earendil-works/pi-ai` Node.js Sidecar。支持免 `npm install` 冷启动，零成本抹平 OpenAI、Anthropic Claude、DeepSeek、Gemini 等所有主流提供商的协议差异与思考档位映射。
3. **上下文智能裁剪与长窗口保护**：
   - 将**工具输出解耦裁剪**（`tool_eviction_threshold_tokens: 20_000`）与**物理模型上下文窗口**彻底解耦。
   - 在高频工具调用下自动压缩修剪旧工具日志，同时完整保留用户与 Assistant 的多轮对话历史至模型真实上下文极限（如 DeepSeek 1M tokens）。
4. **跨 Agent 文件写入互斥排队锁（`FILE_MUTATION_LOCKS`）**：
   - 进程内全局物理路径排队锁，确保多个并发 Agent 或工具在并发写入同一文件时排队串行化执行，杜绝竞态覆盖。
5. **确定性极速插件路由**：
   - `thunder-agent-root` 采用确定性基线（会话与技能常驻）结合关键词意图触发，杜绝死板或昂贵的二次 LLM 路由，消除额外时延。
6. **企业级 STDIO Sidecar 守护进程（`thunder-agent-daemon`）**：
   - 零端口冲突、生命周期天然随父进程绑定，支持并发任务信号量调度、背压 STDOUT 有序推送、协作式暂停（Pause）与反问气泡（Ask User）。

---

## 📦 工作区包概览 (11 Workspace Crates)

| Crate | 路径 | 说明 |
| :--- | :--- | :--- |
| **`thunder-agent-loop`** | [`thunder-agent-loop`](thunder-agent-loop) | **Agent A**：单 Agent 闭环执行引擎，极致轻量与低延迟，支持工具输出解耦裁剪与跨 Agent 文件写入安全锁 |
| **`thunder-pi-bridge`** | [`thunder-pi-bridge`](thunder-pi-bridge) | **LLM 传输桥**：基于 `@earendil-works/pi-ai` 的预打包 Node.js Sidecar，统一多模型方言与流式传输 |
| **`thunder-agent-providers`** | [`thunder-agent-providers`](thunder-agent-providers) | **模型目录**：加载与解析 `models.json` / `auth.json`，提供模型规格缓存与思考档位映射 |
| **`thunder-agent-skills`** | [`thunder-agent-skills`](thunder-agent-skills) | **技能引擎**：扫描并解析 Playbook / SKILL.md，支持意图触发匹配与 120s TTL 全局文件缓存 |
| **`thunder-agent-plugin`** | [`thunder-agent-plugin`](thunder-agent-plugin) | **TS 插件引擎**：TypeScript 单文件插件宿主，原生免编译执行、蓝绿无缝热重载与语法错误免疫 |
| **`thunder-agent-mcp`** | [`thunder-agent-mcp`](thunder-agent-mcp) | **MCP 客户端**：Model Context Protocol 客户端实现，动态发现与无缝桥接远程工具 |
| **`thunder-agent-root`** | [`thunder-agent-root`](thunder-agent-root) | **微内核宿主**：基于基线与触发词动态装配插件，驱动单次任务执行并输出结构化结果 |
| **`thunder-agent-daemon`** | [`thunder-agent-daemon`](thunder-agent-daemon) | **STDIO Sidecar**：面向 Electron 及外部前端的高性能常驻守护进程，支持权限沙箱、协作式暂停与反问 |
| **`thunder-conversation`** | [`thunder-agent-core/conversation`](thunder-agent-core/conversation) | **会话管理**：支持 Fs 原子存储与内存双模、`index.json` 极速索引与多拓扑阶段追踪 |
| **`thunder-orchestra`** | [`thunder-agent-core/thunder-orchestra`](thunder-agent-core/thunder-orchestra) | **Agent B**：多 Agent 编排调度器，提供 Sequential、Parallel、Fan-Out、角色注入与结果聚合 |
| **`thunder-tui`** | [`thunder-agent-core/tui`](thunder-agent-core/tui) | **交互式终端**：基于 Ratatui 的 Claude Code 风格全宽终端，支持打字机流式输出与编排监视器 |

---

## 🚀 快速开始

### 运行环境要求
- **Rust**: 1.80+
- **Node.js**: 20+ (运行 `thunder-pi-bridge` Sidecar 所需，运行时内置打包好的代码，无需 `npm install`)

### 常用命令

```bash
# 1. 克隆仓库
git clone https://github.com/lucacicii/thunder.git
cd thunder

# 2. 启动交互式 TUI 终端
./run.sh

# 3. 启动 STDIO Sidecar 守护进程（供 Electron / 桌面面板调用）
./daemon.sh

# 4. 运行全工作区完整测试套件（11 个包全部测试）
./test.sh
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](LICENSE) 开源许可证。
