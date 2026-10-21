# 🤖 thunder-providers

`thunder-providers` 是 Thunder 的多厂商 LLM 目录解析与流式驱动适配层。

底层基于进程内异步 HTTP 库 `rpi-ai`，直接对接各大模型服务商，**完全摆脱对外部 Node.js 或 Sidecar 进程的依赖**。

---

## 核心职责

1. **进程内多协议流式客户端 (`client.rs`)**：
   * `RpiAiClient`：实现 `thunder-protocol::LLMClientTrait`。
   * 支持协议方言：
     * `openai-completions`：标准 OpenAI Chat Completions 接口；
     * `openai-responses`：OpenAI Responses API；
     * `anthropic-messages`：Anthropic 原生 Messages 流式接口。
   * 自动统计精确的 `prompt_tokens`、`completion_tokens`、`cached_tokens` 与 `cache_write_tokens`。

2. **本地配置与凭据解析 (`config.rs`, `auth.rs`, `source.rs`)**：
   * 自动从 `~/.thunder/models.json`（或 `.thunder/models.json`）读取模型目录定义；
   * 自动从 `~/.thunder/auth.json` 读取 API Key 与访问凭据；
   * 支持模型自定义 `headers`、`base_url`、`context_window`、`max_tokens` 及价格配置。

3. **模型探测与思考档位映射 (`probe.rs`, `catalog.rs`, `convert.rs`)**：
   * `ProviderRegistry`：统一解析管理所有本地可用模型列表（`list_available()`、`resolve(selection_id)`）。
   * `thinking_level_map`：将 `off` / `minimal` / `low` / `medium` / `high` / `xhigh` / `max` 映射为具体厂商特定的 reasoning 参数。

4. **会话标题生成与单次补全 (`naming.rs`, `oneshot.rs`)**：
   * 提供轻量级的单次 LLM 请求能力（`one_shot_text`），并在会话开始后自动基于首轮对话提取生成高内聚的会话标题（`generate_title`）。
