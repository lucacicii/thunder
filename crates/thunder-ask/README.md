# 💬 thunder-ask

`thunder-ask` 是 Thunder 的人机交互问答扩展包（Human-in-the-loop Interactive Questions）。

它为 Agent 提供了在遇到分歧、模糊需求或技术选型时，主动向人类提问并安全挂起等待作答的能力。

---

## 核心职责

1. **问题参数解析与校验 (`protocol.rs`)**：
   * `QuestionItem`、`QuestionOption` 数据结构。
   * `parse_questions`：严格校验模型传入的 `questions` 数组，支持单选与多选（`multi_select`）、提示头标签（`header`）及选项描述。

2. **抽象问答 Handler 契约 (`handler.rs`)**：
   * `UserQuestionHandler` trait：
     ```rust
     #[async_trait]
     pub trait UserQuestionHandler: Send + Sync {
         async fn ask(
             &self,
             task_id: &str,
             session_id: &str,
             question_id: &str,
             questions: &[QuestionItem],
             ctx: &ExecutionContext,
         ) -> Result<QuestionOutcome, String>;
     }
     ```
   * `ChannelQuestionHandler`：基于 `tokio::sync::mpsc` 与 `oneshot`，适用于长周期运行的 Desktop Sidecar 或 TUI。
   * `MockQuestionHandler`：支持自定义闭包或静态答案，用于端到端自动化测试。

3. **问答工具提供者 (`provider.rs`)**：
   * `AskUserToolProvider`：向模型暴露 `core__ask_user_question` 工具。
   * 生成全局唯一 `question_id`，将问题交给已配置的 Handler 并进入异步挂起状态；
   * 人类在前端界面作答后，安全唤醒并将结构化答案返回给大模型。
