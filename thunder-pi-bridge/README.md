# thunder-pi-bridge

> **Thunder 统一 LLM 传输桥 — 通过 [`@earendil-works/pi-ai`](https://www.npmjs.com/package/@earendil-works/pi-ai) Node.js Sidecar 代理模型执行**

[English](README_en.md) | [简体中文](README.md)

`thunder-pi-bridge` 是 Thunder 体系中**唯一的 LLM 传输通道**。它将所有模型调用代理给 `@earendil-works/pi-ai` Node.js Sidecar，以零维护成本抹平所有主流模型供应商的方言差异、思考档位映射与 Token 计量。

---

## 🚀 架构设计

```text
Thunder (Rust)                             Node Sidecar (ESM)
┌──────────────────────┐   NDJSON / stdio   ┌─────────────────────────┐
│ PiAiClient           │ ─────────────────► │ runner/bridge.mjs       │
│  impl LLMClientTrait │ ◄───────────────── │  message/tool mapping   │
│ PiAiBridge (process) │                    │  @earendil-works/pi-ai  │
└──────────────────────┘                    │   stream() → providers  │
                                            └─────────────────────────┘
```

- **`PiAiClient`**：实现 `thunder-agent-loop` 的纯传输契约 `LLMClientTrait`，无需任何 HTTP 或方言适配代码。
- **`PiAiBridge`**：负责 Node.js 子进程的懒加载启动、生命周期守护、崩溃自动重启与优雅关闭。
- **`GLOBAL` 单例**：进程级共享同一个 Sidecar 实例，所有会话与 Agent 单元复用同一长驻通道，消除冷启动开销。

---

## ✨ 核心特性

- **预打包免安装（Pre-bundled）**：`runner/bridge.mjs` 已通过 esbuild 将 `@earendil-works/pi-ai` 及其依赖完整打包。冷拉取仓库后**无需执行 `npm install`**，只要本机装有 Node.js 20+ 即可直接运行。
- **自定义覆盖**：如需指向本地开发版的 pi-ai，可通过环境变量 `$THUNDER_PI_AI_PATH` 指定替换入口。
- **零维护方言矩阵**：DeepSeek、OpenAI o1/o3、Qwen、Moonshot、zAI、Together、OpenRouter、Anthropic、Google、Ollama 等所有供应商差异统一在 `@earendil-works/pi-ai` 内消化。
- **思考档位映射（Thinking Level Map）**：精准翻译各供应商思考强度语义（如 `high` → `max`，`off` → `{ type: "disabled" }`），避免模型意外进入深度推理造成的高延迟。
- **统一 Token 计量**：准确归集输入、输出、思考（Reasoning）与缓存命中 Token 数据。
- **自愈与韧性**：Sidecar 进程崩溃自动拉起，支持任务取消信号传播与流式块空闲超时看门狗。

---

## 📡 协议（NDJSON over stdio）

### Rust ➔ Node

- `{"cmd":"health","id":"..."}`
- `{"cmd":"stream","id":"...","model":{...},"messages":[...],"tools":[...],"thinkingLevel":"..."}`
- `{"cmd":"cancel","id":"..."}`
- `{"cmd":"list_models","id":"..."}`
- `{"cmd":"shutdown"}`

### Node ➔ Rust

- `{"id":"...","type":"ready","version":"...","node":"..."}`
- `{"id":"...","type":"text_delta","delta":"..."}`
- `{"id":"...","type":"reasoning_delta","delta":"..."}`
- `{"id":"...","type":"done","content":"...","toolCalls":[...],"finishReason":"...","usage":{...}}`
- `{"id":"...","type":"error","message":"..."}`
- `{"id":"...","type":"models","models":[...]}`

---

## 🛠️ 自动化测试

```bash
# 运行桥接协议与进程生命周期集成测试
cargo test -p thunder-pi-bridge
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
