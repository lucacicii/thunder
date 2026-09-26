# ⚡ Thunder Agent Providers

> **LLM 模型目录、配置解析层 — 完全兼容 `@earendil-works/pi-ai` 规范**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-providers` 负责加载与解析 `models.json`（模型目录）与 `auth.json`（鉴权信息），提供模型规格发现、上下文窗口缓存、可用性判定与思考档位（Thinking Level）映射。所有 LLM 执行传输统一下沉至 [`thunder-pi-bridge`](../thunder-pi-bridge)（运行 `@earendil-works/pi-ai` 的 Node.js Sidecar），本包**不再持有任何自定义方言适配器**。

---

## 🚀 核心职责

1. **模型目录发现与解析**：
   - 加载用户级 `~/.thunder/models.json`（或 `$THUNDER_CONFIG_DIR/models.json`）与工程级 `<workspace>/.thunder/models.json`。
   - 缓存上下文窗口大小、最大输出 Token、支持能力（tools / reasoning）等模型规格元数据。
2. **鉴权与可用性解析**：
   - 解析 `~/.thunder/auth.json` 与环境变量（`OPENAI_API_KEY`、`ANTHROPIC_API_KEY`、`GEMINI_API_KEY`、`DEEPSEEK_API_KEY`）。
   - 标注每个模型的 `available` 状态，供路由与 UI 提前决策。
3. **思考档位映射**：
   - 将通用的 `thinking_level` 语义对齐到各供应商的专属档位。
4. **思考档位主动探测（Thinking-Level Probing）**：
   - 对本机目录中尚未探测（`thinking_levels_probed != true`）的 reasoning 候选模型，通过内置 HTTP 客户端发起一次轻量探测请求，从供应商返回的错误/枚举信息中推断其真实支持的思考档位（见 `src/probe.rs`）。
   - 探测结果会回写 `models.json` 并标记为已探测，因此后续加载默认跳过网络访问。

> ⚠️ 关于传输职责：本包**不承担模型调用传输**——所有 LLM 执行统一经由 `thunder-pi-bridge`。但它并非"零 HTTP"：`src/probe.rs` 持有 `reqwest` 客户端，仅用于上述一次性的思考档位探测。模型实际推理流量不经由此客户端。

---

## 📡 支持的供应商 API（经由 `@earendil-works/pi-ai`）

| API 协议 | 覆盖供应商 |
| :--- | :--- |
| `openai-completions` | OpenAI、DeepSeek、OpenRouter、Qwen、Moonshot、zAI、Groq、Ollama 等 |
| `anthropic-messages` | Anthropic Claude 3.5 / 3.7 |
| `google-generative-ai` | Google Gemini |

---

## 🛠️ 自动化测试

```bash
# 运行目录解析与元数据缓存测试
cargo test -p thunder-agent-providers
```
