//! `ask_user_question`, terminal-native edition.
//!
//! Same contract as the daemon's tool ([`thunder-agent-daemon`]): the agent
//! loop only sees a normal tool call, while this module owns the interaction —
//! it hands the question to the app as an [`AppEvent::UserQuestion`], parks on
//! a oneshot until the user answers in the question modal, and returns the
//! answer as the tool result.
//!
//! It lives in the host (TUI), not `thunder-agent-loop`, because "ask a human"
//! is product policy.

use crate::event::AppEvent;
use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use thunder_agent_root::prelude::{PluginCapability, PluginManifest, ThunderPlugin, TriggerSpec};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// Default wait for a human answer. Absent an answer the tool returns a timeout
/// result so the loop can continue instead of hanging forever.
const DEFAULT_TIMEOUT_SECS: u64 = 1800;

/// One selectable option of a question.
#[derive(Debug, Clone)]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

/// One question presented to the user.
#[derive(Debug, Clone)]
pub struct AskQuestion {
    pub question: String,
    pub header: Option<String>,
    pub multi_select: bool,
    pub options: Vec<AskOption>,
}

/// A question set handed to the app for interactive answering.
#[derive(Debug)]
pub struct IncomingQuestion {
    pub question_id: String,
    pub questions: Vec<AskQuestion>,
    /// Receives the final answer payload: an object mapping question → answer,
    /// or `Null` when the user dismissed the question.
    pub responder: oneshot::Sender<serde_json::Value>,
}

/// Multi-step modal state for one [`IncomingQuestion`].
///
/// Questions are answered one at a time (queue front → back); collected
/// answers are sent as a single question→answer mapping when the last
/// question is answered, mirroring the daemon's bubble semantics.
#[derive(Debug)]
pub struct PendingQuestion {
    pub question_id: String,
    queue: Vec<AskQuestion>,
    pub answers: Vec<(String, String)>,
    pub selected: usize,
    pub toggled: Vec<usize>,
    pub input: String,
    responder: oneshot::Sender<serde_json::Value>,
}

impl PendingQuestion {
    /// Wrap an incoming question set for interactive answering.
    ///
    /// Returns `None` when the set is empty (already resolved as dismissed —
    /// the tool's parser rejects empty sets, so this is purely defensive).
    pub fn from_incoming(mut incoming: IncomingQuestion) -> Option<Self> {
        if incoming.questions.is_empty() {
            let _ = incoming.responder.send(serde_json::Value::Null);
            return None;
        }
        Some(Self {
            question_id: incoming.question_id,
            queue: std::mem::take(&mut incoming.questions),
            answers: Vec::new(),
            selected: 0,
            toggled: Vec::new(),
            input: String::new(),
            responder: incoming.responder,
        })
    }

    /// The question currently being answered.
    pub fn current(&self) -> Option<&AskQuestion> {
        self.queue.first()
    }

    /// Number of questions still awaiting an answer (including the current one).
    pub fn remaining(&self) -> usize {
        self.queue.len()
    }

    /// Already-collected answers, rendered for display.
    pub fn answered_so_far(&self) -> &[(String, String)] {
        &self.answers
    }

    /// Record an answer for the current question and advance the queue.
    /// Returns the answer payload when the whole set is answered.
    pub fn answer_current(&mut self, answer: String) -> Option<serde_json::Value> {
        let current = self.queue.first()?.clone();
        self.answers.push((current.question.clone(), answer));
        self.queue.remove(0);
        self.selected = 0;
        self.toggled.clear();
        self.input.clear();

        if self.queue.is_empty() {
            let map = self
                .answers
                .iter()
                .map(|(q, a)| (q.clone(), serde_json::Value::String(a.clone())))
                .collect::<serde_json::Map<_, _>>();
            Some(serde_json::Value::Object(map))
        } else {
            None
        }
    }

    /// Resolve the interaction: send the payload (or `Null` for dismissal).
    pub fn resolve(mut self, payload: serde_json::Value) {
        // On dismissal, unanswered questions are simply dropped.
        if !self.queue.is_empty() && payload.is_null() {
            self.queue.clear();
        }
        let _ = self.responder.send(payload);
    }
}

pub struct TuiAskUserTool {
    event_tx: mpsc::UnboundedSender<AppEvent>,
    counter: Arc<AtomicU64>,
    timeout: Duration,
}

impl TuiAskUserTool {
    pub fn new(event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            event_tx,
            counter: Arc::new(AtomicU64::new(1)),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Override the answer wait. Public API for tests.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn parse_questions(args: &serde_json::Value) -> Result<Vec<AskQuestion>, String> {
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
                            o.get("label")
                                .and_then(|l| l.as_str())
                                .map(|label| AskOption {
                                    label: label.to_string(),
                                    description: o
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .map(String::from),
                                })
                        })
                        .collect()
                })
                .unwrap_or_default();

            out.push(AskQuestion {
                question,
                header: item
                    .get("header")
                    .and_then(|v| v.as_str())
                    .map(String::from),
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
impl AgentTool for TuiAskUserTool {
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
        let question_id = format!("tui:q{seq}");

        let (tx, rx) = oneshot::channel();

        info!(
            question_id = %question_id,
            count = questions.len(),
            "Agent is asking the user a question"
        );

        if self
            .event_tx
            .send(AppEvent::UserQuestion(IncomingQuestion {
                question_id: question_id.clone(),
                questions,
                responder: tx,
            }))
            .is_err()
        {
            return Err("Question channel closed before the user saw the question".to_string());
        }

        // Park until answered, dismissed, cancelled, or timed out. The loop is idle here.
        let answers = tokio::select! {
            biased;
            _ = ctx.cancellation_token.cancelled() => {
                return Err("Cancelled while waiting for the user's answer".to_string());
            }
            res = tokio::time::timeout(self.timeout, rx) => {
                match res {
                    Ok(Ok(v)) => v,
                    Ok(Err(_)) => {
                        return Err("Question channel closed before an answer arrived".to_string());
                    }
                    Err(_) => {
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

        if answers.is_null() {
            return Ok("User dismissed the question without answering.".to_string());
        }
        // Prefer the rendered question→answer mapping for the model.
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
        if let Some(s) = answers.as_str() {
            return Ok(s.to_string());
        }
        Ok(answers.to_string())
    }
}

/// Plugin wrapper so the tool rides the existing microkernel registration path
/// (`ThunderPlugin::tools`) instead of growing `RootRunOptions`.
pub struct AskUserPlugin {
    manifest: PluginManifest,
    tool: Arc<TuiAskUserTool>,
}

impl AskUserPlugin {
    pub fn new(tool: TuiAskUserTool) -> Self {
        let manifest = PluginManifest::new(
            "ask_user",
            "Ask User Question",
            "Blocks the agent on a clarifying question rendered as a terminal modal.",
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

/// Terminal-native [`HostUi`] bridge, adapting modal questions to the TUI event loop.
#[derive(Clone)]
pub struct TuiHostUi {
    event_tx: mpsc::UnboundedSender<AppEvent>,
    counter: Arc<AtomicU64>,
}

impl TuiHostUi {
    pub fn new(event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            event_tx,
            counter: Arc::new(AtomicU64::new(1)),
        }
    }
}

#[async_trait]
impl thunder_agent_loop::types::ui::HostUi for TuiHostUi {
    async fn request(
        &self,
        source: thunder_agent_loop::types::ui::UiSource,
        request: thunder_agent_loop::types::ui::UiRequest,
    ) -> thunder_agent_loop::types::ui::UiResponse {
        use thunder_agent_loop::types::ui::{UiRequest, UiResponse, UiSource, DEFAULT_UI_TIMEOUT};

        let seq = self.counter.fetch_add(1, Ordering::SeqCst);
        let question_id = format!("tui:ui:{seq}");
        let (tx, rx) = oneshot::channel();

        let ask_question = match &request {
            UiRequest::Select { title, options, .. } => {
                let ask_opts = options
                    .iter()
                    .map(|o| AskOption {
                        label: o.clone(),
                        description: None,
                    })
                    .collect();
                AskQuestion {
                    question: title.clone(),
                    header: Some(if source == UiSource::Host {
                        "Approval".to_string()
                    } else {
                        "Plugin".to_string()
                    }),
                    multi_select: false,
                    options: ask_opts,
                }
            }
            UiRequest::Confirm { title, message, .. } => AskQuestion {
                question: if message.is_empty() {
                    title.clone()
                } else {
                    format!("{title}\n{message}")
                },
                header: Some("Confirm".to_string()),
                multi_select: false,
                options: vec![
                    AskOption {
                        label: "Confirm".to_string(),
                        description: None,
                    },
                    AskOption {
                        label: "Cancel".to_string(),
                        description: None,
                    },
                ],
            },
            UiRequest::Input {
                title, placeholder, ..
            } => AskQuestion {
                question: title.clone(),
                header: placeholder.clone(),
                multi_select: false,
                options: Vec::new(),
            },
            UiRequest::Editor { title, prefill, .. } => AskQuestion {
                question: title.clone(),
                header: prefill.clone(),
                multi_select: false,
                options: Vec::new(),
            },
        };

        let incoming = IncomingQuestion {
            question_id,
            questions: vec![ask_question],
            responder: tx,
        };

        if self
            .event_tx
            .send(AppEvent::UserQuestion(incoming))
            .is_err()
        {
            return UiResponse::Cancelled;
        }

        let timeout_dur = match request {
            UiRequest::Select {
                timeout_ms: Some(ms),
                ..
            }
            | UiRequest::Confirm {
                timeout_ms: Some(ms),
                ..
            }
            | UiRequest::Input {
                timeout_ms: Some(ms),
                ..
            }
            | UiRequest::Editor {
                timeout_ms: Some(ms),
                ..
            } => Duration::from_millis(ms),
            _ => DEFAULT_UI_TIMEOUT,
        };

        match tokio::time::timeout(timeout_dur, rx).await {
            Ok(Ok(val)) => {
                if val.is_null() {
                    UiResponse::Cancelled
                } else if let Some(obj) = val.as_object() {
                    if let Some(ans) = obj.values().next().and_then(|v| v.as_str()) {
                        match request {
                            UiRequest::Confirm { .. } => UiResponse::Confirmed {
                                confirmed: ans.eq_ignore_ascii_case("confirm")
                                    || ans == "yes"
                                    || ans == "true",
                            },
                            _ => UiResponse::value(ans.to_string()),
                        }
                    } else {
                        UiResponse::Cancelled
                    }
                } else {
                    UiResponse::Cancelled
                }
            }
            _ => UiResponse::Cancelled,
        }
    }

    fn notify(
        &self,
        _source: thunder_agent_loop::types::ui::UiSource,
        _message: &str,
        _level: thunder_agent_loop::types::ui::NotifyLevel,
    ) {
    }

    fn set_status(&self, _key: &str, _text: Option<String>) {}
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
    use thunder_agent_loop::types::tool::AgentTool;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            tool_call_id: "call_1".to_string(),
            turn: 1,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            ..Default::default()
        }
    }

    fn args(questions: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "questions": questions })
    }

    #[test]
    fn parse_rejects_missing_or_empty_questions() {
        assert!(TuiAskUserTool::parse_questions(&serde_json::json!({})).is_err());
        assert!(TuiAskUserTool::parse_questions(&args(serde_json::json!([]))).is_err());
    }

    #[test]
    fn parse_requires_question_text_and_maps_fields() {
        assert!(TuiAskUserTool::parse_questions(&args(serde_json::json!([{}]))).is_err());

        let parsed = TuiAskUserTool::parse_questions(&args(serde_json::json!([
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
        let parsed = TuiAskUserTool::parse_questions(&args(serde_json::json!([
            { "question": "q", "multi_select": true }
        ])))
        .unwrap();
        assert!(parsed[0].multi_select);
    }

    #[tokio::test]
    async fn pending_question_collects_multi_question_answers() {
        let (tx, rx) = oneshot::channel();
        let mut pending = PendingQuestion::from_incoming(IncomingQuestion {
            question_id: "tui:q1".to_string(),
            questions: vec![
                AskQuestion {
                    question: "Layer?".to_string(),
                    header: None,
                    multi_select: false,
                    options: vec![],
                },
                AskQuestion {
                    question: "Scope?".to_string(),
                    header: None,
                    multi_select: false,
                    options: vec![],
                },
            ],
            responder: tx,
        })
        .expect("non-empty question set");

        assert_eq!(pending.remaining(), 2);
        assert!(pending.answer_current("renderer".to_string()).is_none());
        assert_eq!(pending.remaining(), 1);
        let payload = pending
            .answer_current("full repo".to_string())
            .expect("last answer resolves the set");
        pending.resolve(payload);

        let received = rx.await.unwrap();
        let map = received.as_object().unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map["Layer?"].as_str(), Some("renderer"));
        assert_eq!(map["Scope?"].as_str(), Some("full repo"));
    }

    #[tokio::test]
    async fn empty_question_sets_are_dismissed_immediately() {
        let (tx, rx) = oneshot::channel();
        assert!(PendingQuestion::from_incoming(IncomingQuestion {
            question_id: "tui:q1".to_string(),
            questions: vec![],
            responder: tx,
        })
        .is_none());
        assert!(rx.await.unwrap().is_null());
    }

    #[tokio::test]
    async fn dismissal_sends_null() {
        let (tx, rx) = oneshot::channel();
        let pending = PendingQuestion::from_incoming(IncomingQuestion {
            question_id: "tui:q1".to_string(),
            questions: vec![AskQuestion {
                question: "q".to_string(),
                header: None,
                multi_select: false,
                options: vec![],
            }],
            responder: tx,
        })
        .expect("non-empty question set");
        pending.resolve(serde_json::Value::Null);
        assert!(rx.await.unwrap().is_null());
    }

    #[tokio::test]
    async fn tool_answers_through_the_event_channel() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let tool = TuiAskUserTool::new(event_tx).with_timeout(Duration::from_secs(5));

        let handle = tokio::spawn(async move {
            tool.execute(args(serde_json::json!([{ "question": "Layer?" }])), &ctx())
                .await
        });

        let incoming = match event_rx.recv().await {
            Some(AppEvent::UserQuestion(incoming)) => incoming,
            other => panic!("expected UserQuestion event, got {other:?}"),
        };
        let mut pending = PendingQuestion::from_incoming(incoming).expect("non-empty set");
        let payload = pending
            .answer_current("renderer".to_string())
            .expect("single question resolves immediately");
        pending.resolve(payload);

        let out = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("answer unblocks the tool")
            .unwrap()
            .expect("answering is not an error");
        assert!(out.contains("Layer?"), "renders the question: {out}");
        assert!(out.contains("renderer"), "renders the answer: {out}");
    }

    #[tokio::test]
    async fn cancellation_releases_the_waiting_tool() {
        let (event_tx, _rx) = mpsc::unbounded_channel();
        let tool = TuiAskUserTool::new(event_tx).with_timeout(Duration::from_secs(30));

        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = ToolExecutionContext {
            tool_call_id: "call_1".to_string(),
            turn: 1,
            cancellation_token: cancel.clone(),
            ..Default::default()
        };

        let handle = tokio::spawn(async move {
            tool.execute(args(serde_json::json!([{ "question": "q" }])), &ctx)
                .await
        });

        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("cancel unblocks promptly")
            .unwrap();
        assert!(res.is_err(), "cancellation surfaces as an error");
    }

    #[tokio::test]
    async fn tui_host_ui_select_bridges_to_user_question_and_returns_choice() {
        use thunder_agent_loop::types::ui::{HostUi, UiRequest, UiResponse, UiSource};

        let (event_tx, mut rx) = mpsc::unbounded_channel();
        let ui = TuiHostUi::new(event_tx);

        let handle = tokio::spawn(async move {
            ui.request(
                UiSource::Host,
                UiRequest::Select {
                    title: "执行 bash".to_string(),
                    options: vec!["允许一次".to_string(), "拒绝".to_string()],
                    timeout_ms: Some(5000),
                },
            )
            .await
        });

        let event = rx.recv().await.expect("event received");
        let incoming = match event {
            AppEvent::UserQuestion(q) => q,
            _ => panic!("expected UserQuestion"),
        };

        assert_eq!(incoming.questions.len(), 1);
        assert_eq!(incoming.questions[0].question, "执行 bash");
        assert_eq!(
            incoming.questions[0].header.as_deref(),
            Some("Approval"),
            "the dialog's own chrome is English even when the caller is not"
        );
        assert_eq!(incoming.questions[0].options.len(), 2);

        let mut pending = PendingQuestion::from_incoming(incoming).expect("pending question");
        let payload = pending
            .answer_current("允许一次".to_string())
            .expect("resolves immediately");
        pending.resolve(payload);

        let response = handle.await.unwrap();
        assert_eq!(response, UiResponse::value("允许一次"));
    }

    #[tokio::test]
    async fn tui_confirm_dialog_is_english_end_to_end() {
        use thunder_agent_loop::types::ui::{HostUi, UiRequest, UiResponse, UiSource};

        let (event_tx, mut rx) = mpsc::unbounded_channel();
        let ui = TuiHostUi::new(event_tx);

        let handle = tokio::spawn(async move {
            ui.request(
                UiSource::Host,
                UiRequest::Confirm {
                    title: "Run bash".to_string(),
                    message: "rm -rf build/".to_string(),
                    timeout_ms: Some(5000),
                },
            )
            .await
        });

        let incoming = match rx.recv().await.expect("event received") {
            AppEvent::UserQuestion(q) => q,
            _ => panic!("expected UserQuestion"),
        };
        let question = &incoming.questions[0];
        assert_eq!(question.header.as_deref(), Some("Confirm"));
        let labels: Vec<&str> = question
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        assert_eq!(labels, vec!["Confirm", "Cancel"]);

        // Picking the first option is what makes it a confirmation.
        let mut pending = PendingQuestion::from_incoming(incoming).expect("pending question");
        let payload = pending
            .answer_current("Confirm".to_string())
            .expect("resolves immediately");
        pending.resolve(payload);

        let response = handle.await.unwrap();
        assert_eq!(response, UiResponse::Confirmed { confirmed: true });
    }
}
