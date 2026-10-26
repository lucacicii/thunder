use crate::handler::UserQuestionHandler;
use crate::protocol::parse_questions;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use thunder_protocol::{ExecutionContext, ToolDescriptor, ToolExecutionError, ToolProvider};

pub struct AskUserToolProvider {
    handler: Arc<dyn UserQuestionHandler>,
    counter: Arc<AtomicU64>,
}

impl AskUserToolProvider {
    pub fn new(handler: Arc<dyn UserQuestionHandler>) -> Self {
        Self {
            handler,
            counter: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn shared(handler: Arc<dyn UserQuestionHandler>) -> Arc<Self> {
        Arc::new(Self::new(handler))
    }
}

#[async_trait]
impl ToolProvider for AskUserToolProvider {
    fn provider_id(&self) -> &str {
        "ask_user"
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
        Ok(vec![ToolDescriptor::new_core(
            "ask_user_question",
            "Ask the human user one or more structured questions with options when a decision or clarification is required before proceeding. The tool pauses execution until the user answers.",
            json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "description": "List of questions to ask the user.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {
                                    "type": "string",
                                    "description": "The exact question text."
                                },
                                "header": {
                                    "type": "string",
                                    "description": "Short category or chip label (e.g. 'Auth' or 'Database')."
                                },
                                "multi_select": {
                                    "type": "boolean",
                                    "description": "Whether multiple choices can be selected simultaneously (default: false)."
                                },
                                "options": {
                                    "type": "array",
                                    "description": "Selectable choices presented to the user.",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": {
                                                "type": "string",
                                                "description": "Short label of the choice (e.g. 'PostgreSQL')."
                                            },
                                            "description": {
                                                "type": "string",
                                                "description": "Detailed explanation of what this choice entails."
                                            }
                                        },
                                        "required": ["label"]
                                    }
                                }
                            },
                            "required": ["question", "options"]
                        }
                    }
                },
                "required": ["questions"]
            }),
        )])
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        let logical = name.strip_prefix("core__").unwrap_or(name);
        if logical != "ask_user_question" {
            return Err(ToolExecutionError::ToolNotFound(name.to_string()));
        }

        let questions =
            parse_questions(&arguments).map_err(ToolExecutionError::SerializationError)?;

        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let question_id = format!("q-{seq}");

        // The task id is the handler's business, not the provider's: the only id in
        // scope here is the session id, and deriving a task id from it names a task no
        // host has ever started.
        let outcome = self
            .handler
            .ask(&ctx.session_id, &question_id, &questions, ctx)
            .await
            .map_err(|e| ToolExecutionError::NonZeroExit { code: 1, stderr: e })?;

        if outcome.cancelled {
            return Err(ToolExecutionError::NonZeroExit {
                code: 1,
                stderr: "Question was dismissed/cancelled by the user without answering."
                    .to_string(),
            });
        }

        serde_json::to_string_pretty(&outcome.answers)
            .map_err(|e| ToolExecutionError::SerializationError(e.to_string()))
    }
}
