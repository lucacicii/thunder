//! `ask_user_question`: let the agent block on a human answer.
//!
//! The agent loop stays pure — it only sees a normal tool call. This tool owns
//! the interaction: it emits a `user_question` message to the host, parks on a
//! oneshot until the panel answers, and returns the answer as the tool result.
//!
//! It lives in the daemon (a host), not in `thunder-agent-loop`, because
//! "ask a human" is product policy — exactly the kind of thing A must not know.

use crate::protocol::{DaemonResponse, QuestionItem};
use crate::service::QuestionOutcome;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use thunder_agent_root::prelude::{PluginCapability, PluginManifest, ThunderPlugin, TriggerSpec};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{info, warn};

/// Default wait for a human answer. Absent an answer the tool returns a timeout
/// result so the loop can continue instead of hanging forever.
const DEFAULT_TIMEOUT_SECS: u64 = 1800;

/// Shared routing table for pending questions, owned by the daemon.
pub type PendingQuestions = Arc<Mutex<HashMap<String, oneshot::Sender<QuestionOutcome>>>>;

pub struct AskUserQuestionTool {
    task_id: String,
    session_id: String,
    output_tx: mpsc::Sender<String>,
    pending: PendingQuestions,
    counter: Arc<AtomicU64>,
    timeout: Duration,
}

impl AskUserQuestionTool {
    pub fn new(
        task_id: String,
        session_id: String,
        output_tx: mpsc::Sender<String>,
        pending: PendingQuestions,
    ) -> Self {
        Self {
            task_id,
            session_id,
            output_tx,
            pending,
            counter: Arc::new(AtomicU64::new(1)),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Override the answer wait. Public API for hosts/tests; unused in-crate today.
    #[allow(dead_code)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn parse_questions(args: &serde_json::Value) -> Result<Vec<QuestionItem>, String> {
        let raw = args
            .get("questions")
            .and_then(|v| v.as_array())
            .ok_or("Missing required field 'questions' (array)")?;

        if raw.is_empty() {
            return Err("'questions' must not be empty".to_string());
        }

        let mut out = Vec::with_capacity(raw.len());
        for (idx, item) in raw.iter().enumerate() {
            let question = item
                .get("question")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("questions[{idx}] is missing 'question'"))?
                .to_string();

            let options = item
                .get("options")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|o| {
                            o.get("label").and_then(|l| l.as_str()).map(|label| {
                                crate::protocol::QuestionOption {
                                    label: label.to_string(),
                                    description: o
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .map(String::from),
                                }
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            out.push(QuestionItem {
                question,
                header: item.get("header").and_then(|v| v.as_str()).map(String::from),
                multi_select: item
                    .get("multiSelect")
                    .or_else(|| item.get("multi_select"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                options,
            });
        }

        Ok(out)
    }
}

#[async_trait]
impl AgentTool for AskUserQuestionTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "ask_user_question",
            "Ask the user one or more clarifying questions and wait for their answer. \
             Use this instead of guessing when a decision materially changes the outcome.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "description": "Questions to present to the user.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": { "type": "string", "description": "The question text." },
                                "header": { "type": "string", "description": "Short label." },
                                "multiSelect": { "type": "boolean", "description": "Allow multiple selections." },
                                "options": {
                                    "type": "array",
                                    "description": "Selectable options. Omit for free-form answers.",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string" },
                                            "description": { "type": "string" }
                                        },
                                        "required": ["label"]
                                    }
                                }
                            },
                            "required": ["question"]
                        }
                    }
                },
                "required": ["questions"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let questions = Self::parse_questions(&args)?;

        let seq = self.counter.fetch_add(1, Ordering::SeqCst);
        let question_id = format!("{}:q{}", self.task_id, seq);

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(question_id.clone(), tx);

        info!(
            task_id = %self.task_id,
            question_id = %question_id,
            count = questions.len(),
            "Agent is asking the user a question"
        );

        // Emit to the host; the panel turns this into a bubble.
        {
            let msg = DaemonResponse::UserQuestion {
                task_id: self.task_id.clone(),
                session_id: Some(self.session_id.clone()),
                question_id: question_id.clone(),
                questions,
            };
            let mut line = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
            line.push('\n');
            let _ = self.output_tx.send(line).await;
        }

        // Park until answered, cancelled, or timed out. The loop is idle here.
        let outcome = tokio::select! {
            biased;
            _ = ctx.cancellation_token.cancelled() => {
                self.pending.lock().await.remove(&question_id);
                return Err("Cancelled while waiting for the user's answer".to_string());
            }
            res = tokio::time::timeout(self.timeout, rx) => {
                match res {
                    Ok(Ok(o)) => o,
                    Ok(Err(_)) => {
                        return Err("Question channel closed before an answer arrived".to_string());
                    }
                    Err(_) => {
                        self.pending.lock().await.remove(&question_id);
                        warn!(question_id = %question_id, "Timed out waiting for user answer");
                        return Err(format!(
                            "No answer received within {}s. Proceed with clearly-stated assumptions, \
                             or ask again if the decision is blocking.",
                            self.timeout.as_secs()
                        ));
                    }
                }
            }
        };

        match outcome {
            QuestionOutcome::Answered(answers) => {
                // Prefer a plain string when the panel sent one; otherwise render
                // the question→answer mapping for the model.
                if let Some(s) = answers.as_str() {
                    return Ok(s.to_string());
                }
                if let Some(map) = answers.as_object() {
                    if map.is_empty() {
                        return Ok("User dismissed the question without answering.".to_string());
                    }
                    let rendered = map
                        .iter()
                        .map(|(q, a)| {
                            let answer = a
                                .as_str()
                                .map(String::from)
                                .unwrap_or_else(|| a.to_string());
                            format!("Q: {q}\nA: {answer}")
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    return Ok(format!("User answered:\n\n{rendered}"));
                }
                Ok(answers.to_string())
            }
            QuestionOutcome::Cancelled => {
                Ok("User dismissed the question without answering.".to_string())
            }
        }
    }
}

/// Plugin wrapper so the tool rides the existing microkernel registration path
/// (`ThunderPlugin::tools`) instead of growing `RootRunOptions`.
pub struct AskUserPlugin {
    manifest: PluginManifest,
    tool: Arc<AskUserQuestionTool>,
}

impl AskUserPlugin {
    pub fn new(tool: AskUserQuestionTool) -> Self {
        let manifest = PluginManifest::new(
            "ask_user",
            "Ask User Question",
            "Blocks the agent on a clarifying question rendered as a panel bubble.",
            "0.1.0",
        )
        .with_capability(PluginCapability::ToolProvider)
        .with_triggers(TriggerSpec::always());

        Self {
            manifest,
            tool: Arc::new(tool),
        }
    }
}

#[async_trait]
impl ThunderPlugin for AskUserPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        vec![self.tool.clone()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use thunder_agent_loop::types::tool::AgentTool;
    use tokio::sync::Mutex;

    fn tool_with(
        pending: PendingQuestions,
        timeout: Duration,
    ) -> AskUserQuestionTool {
        let (output_tx, _rx) = mpsc::channel(32);
        AskUserQuestionTool::new(
            "task-1".to_string(),
            "sess-1".to_string(),
            output_tx,
            pending,
        )
        .with_timeout(timeout)
    }

    fn new_pending() -> PendingQuestions {
        Arc::new(Mutex::new(HashMap::new()))
    }

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            tool_call_id: "call_1".to_string(),
            turn: 1,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn args(questions: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "questions": questions })
    }

    #[test]
    fn parse_rejects_missing_or_empty_questions() {
        assert!(AskUserQuestionTool::parse_questions(&serde_json::json!({})).is_err());
        assert!(AskUserQuestionTool::parse_questions(&args(serde_json::json!([]))).is_err());
    }

    #[test]
    fn parse_requires_question_text_and_maps_fields() {
        assert!(AskUserQuestionTool::parse_questions(&args(serde_json::json!([{}]))).is_err());

        let parsed = AskUserQuestionTool::parse_questions(&args(serde_json::json!([
            {
                "question": "Which layer?",
                "header": "Scope",
                "multiSelect": true,
                "options": [
                    { "label": "renderer", "description": "UI only" },
                    { "label": "main" }
                ]
            }
        ])))
        .expect("valid payload");

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].question, "Which layer?");
        assert_eq!(parsed[0].header.as_deref(), Some("Scope"));
        assert!(parsed[0].multi_select);
        assert_eq!(parsed[0].options.len(), 2);
        assert_eq!(parsed[0].options[0].label, "renderer");
        assert_eq!(parsed[0].options[0].description.as_deref(), Some("UI only"));
    }

    #[test]
    fn parse_accepts_snake_case_multi_select() {
        let parsed = AskUserQuestionTool::parse_questions(&args(serde_json::json!([
            { "question": "q", "multi_select": true }
        ])))
        .unwrap();
        assert!(parsed[0].multi_select);
    }

    #[tokio::test]
    async fn answering_returns_the_mapped_answer_and_clears_the_slot() {
        let pending = new_pending();
        let tool = tool_with(Arc::clone(&pending), Duration::from_secs(30));

        let question = args(serde_json::json!([
            { "question": "Which layer?", "options": [{ "label": "renderer" }] }
        ]));

        let handle = tokio::spawn(async move { tool.execute(question, &ctx()).await });

        // Wait for the tool to register its pending question.
        let question_id = loop {
            let guard = pending.lock().await;
            if let Some(id) = guard.keys().next().cloned() {
                break id;
            }
            drop(guard);
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        assert_eq!(question_id, "task-1:q1", "id is task-scoped and sequential");

        let tx = pending.lock().await.remove(&question_id).expect("pending slot");
        tx.send(QuestionOutcome::Answered(serde_json::json!({
            "Which layer?": "renderer"
        })))
        .expect("deliver answer");

        let out = handle.await.unwrap().expect("tool returns the answer");
        assert!(out.contains("Which layer?"), "renders the question: {out}");
        assert!(out.contains("renderer"), "renders the answer: {out}");
        assert!(pending.lock().await.is_empty(), "slot is released after answering");
    }

    #[tokio::test]
    async fn dismissal_reports_that_no_answer_was_given() {
        let pending = new_pending();
        let tool = tool_with(Arc::clone(&pending), Duration::from_secs(30));
        let question = args(serde_json::json!([{ "question": "q" }]));

        let handle = tokio::spawn(async move { tool.execute(question, &ctx()).await });

        let question_id = loop {
            let guard = pending.lock().await;
            if let Some(id) = guard.keys().next().cloned() {
                break id;
            }
            drop(guard);
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        let tx = pending.lock().await.remove(&question_id).unwrap();
        tx.send(QuestionOutcome::Cancelled).unwrap();

        let out = handle.await.unwrap().expect("dismissal is not an error");
        assert!(out.to_lowercase().contains("dismissed"), "got: {out}");
    }

    #[tokio::test]
    async fn cancellation_releases_the_waiting_tool() {
        let pending = new_pending();
        let tool = tool_with(Arc::clone(&pending), Duration::from_secs(30));
        let question = args(serde_json::json!([{ "question": "q" }]));

        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = ToolExecutionContext {
            tool_call_id: "call_1".to_string(),
            turn: 1,
            cancellation_token: cancel.clone(),
        };

        let handle = tokio::spawn(async move { tool.execute(question, &ctx).await });

        // Let it register, then cancel.
        loop {
            if !pending.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel.cancel();

        let res = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("cancel unblocks promptly")
            .unwrap();
        assert!(res.is_err(), "cancellation surfaces as an error");
        assert!(pending.lock().await.is_empty(), "slot cleaned up on cancel");
    }

    #[tokio::test]
    async fn timeout_returns_guidance_instead_of_hanging() {
        let pending = new_pending();
        let tool = tool_with(Arc::clone(&pending), Duration::from_millis(60));
        let question = args(serde_json::json!([{ "question": "q" }]));

        let out = tokio::time::timeout(
            Duration::from_secs(2),
            tool.execute(question, &ctx()),
        )
        .await
        .expect("tool must self-terminate on timeout")
        .expect_err("timeout is reported as an error");

        assert!(out.contains("No answer received"), "got: {out}");
        assert!(pending.lock().await.is_empty(), "slot cleaned up on timeout");
    }

    #[tokio::test]
    async fn concurrent_questions_get_distinct_ids() {
        let pending = new_pending();
        let tool = Arc::new(tool_with(Arc::clone(&pending), Duration::from_secs(30)));

        let mut handles = Vec::new();
        for _ in 0..3 {
            let t = Arc::clone(&tool);
            handles.push(tokio::spawn(async move {
                t.execute(args(serde_json::json!([{ "question": "q" }])), &ctx())
                    .await
            }));
        }

        // Collect ids until all three are registered.
        let ids = loop {
            let guard = pending.lock().await;
            if guard.len() == 3 {
                break guard.keys().cloned().collect::<Vec<_>>();
            }
            drop(guard);
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        assert_eq!(ids.len(), 3);
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), 3, "ids must be unique: {ids:?}");

        for id in ids {
            let tx = pending.lock().await.remove(&id).unwrap();
            tx.send(QuestionOutcome::Cancelled).unwrap();
        }
        for h in handles {
            let _ = h.await;
        }
    }
}
