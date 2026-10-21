# 🧠 thunder-core

`thunder-core` 是 Thunder 的微内核调度核心。

它恪守微内核纯粹性（Kernel Purity）设计哲学：**仅负责推理状态机、前置网关、上下文预算管理与可组合中间件管线，不包含任何业务 I/O 与特定工具硬编码**。

---

## 核心职责

1. **ReAct Agent Loop 状态机 (`agent_loop.rs`)**：
   * 驱动 `TurnStart (冻结快照) -> LLM 推理 -> 工具分发 (直接调用或网关路由) -> 结果归一化 -> 终止判定` 完整闭环。
   * 支持通过 `with_cancellation_token`、`with_pause_gate`、`with_steer_queues`、`with_middleware` 进行无损控制。

2. **KV-Cache 护盾与双层暴露网关 (`gateway.rs`)**：
   * 顶层 `tools` 参数在会话周期内保持静态（仅暴露 7 个核心工具 + 2 个元工具）：
     * `thunder__discover_tools`：元工具，按关键字搜索当前快照中的全量工具并返回完整 Schema；
     * `thunder__call_tool`：元工具，统一网关路由分发至 `CatalogSnapshot` 中的任意动态或核心工具。
   * 动态加载工具通过在 Turn 边界向消息末尾注入 `<system_notification>[Registry Update]` 实现渐进式披露，顶层 `tools` 数组 100% 保持字节稳定。

3. **上下文管理与 Pi-Style Checkpoint 压缩 (`context.rs`, `pruning/*`)**：
   * `token_estimator.rs`：零分配 CJK 感知 Token 估算器（中文 `~1.5 tokens/字`，ASCII `~3.7 chars/token`，图片 `1600 tokens/图`）。
   * `context.rs`：`ContextBuffer` 增量维护消息历史与 Token 总量。
   * `checkpoint.rs` & `strategy.rs`：阶跃式压缩。未达到阈值时历史消息一字不改；达到阈值时自动跨过 Tool 结果找到安全 User 边界（绝不拆散 Tool 配对），提取文件列表并生成结构化 `context_checkpoint` 消息，支持 `emergency_trim` 兜底。
   * `error_detector.rs`：跨厂商识别 OpenAI / Anthropic / DeepSeek / Qwen 的爆窗报错，动态下调阈值、立即压缩并自愈重试。

4. **Prompt Cache 保温器 (`cache_warmer.rs`)**：
   * 根据模型价格与 TTL 进行精算，在期望收益 $\ge \$0.05$ 时，于 90% TTL 处自动发送 `max_tokens = 1` 请求为缓存续命。

5. **纯粹的中间件管线 (`middleware.rs`)**：
   * 提供抽象的 `ToolMiddleware` trait 与 `MiddlewarePipeline`。无论是直接调用顶层工具还是通过 `thunder__call_tool` 网关路由的调用，均强制经过已配置的中间件管线。

6. **会话持久化存储 (`conversation.rs`)**：
   * 提供 `Conversation`、`ConversationSummary`、`FsConversationStore`（兼容 `~/.thunder/conversations/index.json` 与 `<session_id>/conversation.json` 的跨进程读合并写）与 `MemoryConversationStore`。

7. **控制原语 (`loop_guard.rs`, `pause.rs`, `steer.rs`)**：
   * `LoopGuard`：死循环检测与连续报错熔断器。
   * `PauseGate`：在工具执行边界协作式暂停与恢复。
   * `SteerQueues`：支持在当前轮次结束后插入纠偏消息（`steering`）或在任务完成前追加后续指令（`follow_up`）。
