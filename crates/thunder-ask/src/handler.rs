use crate::protocol::QuestionItem;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thunder_protocol::ExecutionContext;
use tokio::sync::{mpsc, oneshot};

/// Result outcome sent back by a host answering or dismissing a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOutcome {
    pub answers: Value,
    pub cancelled: bool,
}

impl QuestionOutcome {
    pub fn answer(answers: Value) -> Self {
        Self {
            answers,
            cancelled: false,
        }
    }

    pub fn cancelled() -> Self {
        Self {
            answers: Value::Null,
            cancelled: true,
        }
    }
}

/// Prompt package sent to an interactive host handler.
#[derive(Debug)]
pub struct QuestionPrompt {
    pub task_id: String,
    pub session_id: String,
    pub question_id: String,
    pub questions: Vec<QuestionItem>,
    pub responder: oneshot::Sender<QuestionOutcome>,
}

/// Abstract handler contract for presenting questions to a human and awaiting the answer.
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

/// Mock question handler for deterministic tests.
pub struct MockQuestionHandler {
    answer_generator: Box<dyn Fn(&str, &[QuestionItem]) -> QuestionOutcome + Send + Sync>,
}

impl MockQuestionHandler {
    pub fn new<F>(generator: F) -> Self
    where
        F: Fn(&str, &[QuestionItem]) -> QuestionOutcome + Send + Sync + 'static,
    {
        Self {
            answer_generator: Box::new(generator),
        }
    }

    pub fn constant(answers: Value) -> Self {
        Self::new(move |_, _| QuestionOutcome::answer(answers.clone()))
    }
}

#[async_trait]
impl UserQuestionHandler for MockQuestionHandler {
    async fn ask(
        &self,
        _task_id: &str,
        _session_id: &str,
        question_id: &str,
        questions: &[QuestionItem],
        _ctx: &ExecutionContext,
    ) -> Result<QuestionOutcome, String> {
        Ok((self.answer_generator)(question_id, questions))
    }
}

/// Channel-based question handler for long-running daemon/TUI hosts.
pub struct ChannelQuestionHandler {
    prompt_tx: mpsc::Sender<QuestionPrompt>,
}

impl ChannelQuestionHandler {
    pub fn new(prompt_tx: mpsc::Sender<QuestionPrompt>) -> Self {
        Self { prompt_tx }
    }
}

#[async_trait]
impl UserQuestionHandler for ChannelQuestionHandler {
    async fn ask(
        &self,
        task_id: &str,
        session_id: &str,
        question_id: &str,
        questions: &[QuestionItem],
        ctx: &ExecutionContext,
    ) -> Result<QuestionOutcome, String> {
        let (tx, rx) = oneshot::channel();
        let prompt = QuestionPrompt {
            task_id: task_id.to_string(),
            session_id: session_id.to_string(),
            question_id: question_id.to_string(),
            questions: questions.to_vec(),
            responder: tx,
        };

        self.prompt_tx
            .send(prompt)
            .await
            .map_err(|_| "Failed to dispatch question to UI: receiver closed".to_string())?;

        tokio::time::timeout(ctx.timeout, rx)
            .await
            .map_err(|_| "Timed out waiting for user answer".to_string())?
            .map_err(|_| "Question responder dropped before answering".to_string())
    }
}
