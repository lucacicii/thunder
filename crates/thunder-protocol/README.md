# 📜 thunder-protocol

`thunder-protocol` 是 Thunder Agent 体系的基础纯契约层，**对工作区内部其他 Crate 零依赖**。

它统一定义了跨进程 IPC、网络流式传输和调度核心所依赖的所有数据结构与通信接口。

---

## 核心职责

1. **统一工具 Schema 与 URI 体系 (`schema.rs`)**：
   * `ToolDescriptor`：统一抽象工具的名称、描述、JSON Schema 与 canonical URI。
   * `ToolNamespace`：统一内部 URI 规范（`core://<name>`, `mcp://<server>/<name>`, `script://<name>`），并提供向 LLM 安全扁平名（`core__*`, `mcp__*__*`, `script__*`）的双向解析。
   * 命名空间优先级规范：`Local Script (3) > External MCP (2) > Core (1)`。
   * `ExecutionContext`：携带 `session_id`, `workspace_cwd`, `timeout` 的上下文对象。
   * `ToolExecutionResult`：三元故障归一化枚举（`Success`, `ToolError`, `SystemFault`）及标准化错误消息转换。
   * `ToolProvider` trait：所有工具提供方（MCP Client, Script Runner, In-process Crate）实现的统一异步接口。

2. **MCP 与上下文信封扩展 (`mcp.rs`)**：
   * JSON-RPC 2.0 基础报文类型：`JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcError`, `JsonRpcNotification`。
   * `ContextEnvelopeMeta` (`_meta`)：在 `tools/call` 请求中透传 `session_id`, `workspace_cwd`, `timeout_ms`，赋予无状态长连接子进程多会话工作区隔离能力。
   * `McpToolInfo`, `McpListToolsResult`, `McpCallToolResult`。

3. **Wire 消息模型 (`message.rs`)**：
   * `ChatMessage`：支持 `System`, `User` (含 `ContentPart::Image` 多模态), `Assistant` (含 `reasoning` 思考流与 `stats`), `Tool`。
   * `ToolDefinition`, `FunctionDefinition`。
   * `PromptCacheWarmSettings`：模型提示词缓存价格与 TTL 结构体。

4. **流式传输契约 (`stream.rs`)**：
   * `LLMClientTrait`：流式 LLM HTTP 驱动抽象。
   * `LLMStreamChunk`：分块枚举（`Token`, `ReasoningToken`, `ToolCallChunk`, `Completed`）。
   * `ChatRequestOptions`。

5. **统一事件总线 (`event.rs`)**：
   * `ThunderEvent`：内核总线通知。
   * `AgentEvent` / `ObservedEvent`：细粒度前端流式帧（`TurnStart`, `TokenDelta`, `ReasoningDelta`, `ToolCallReady`, `ToolExecStart`, `ToolExecResult`, `TurnEnd`, `ContextCompacted`, `SteerAccepted`, `LoopComplete`），对接桌面端与 TUI。

6. **统一数据根定位 (`paths.rs`)**：
   * 遵循 `THUNDER_CONFIG_DIR` 环境变量 $\to$ `~/.thunder` $\to$ `/tmp/.thunder` 单一来源解析规则。
