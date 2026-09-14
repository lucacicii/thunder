use crate::types::Conversation;
use thunder_agent_loop::core::context::ContextBuffer;
use thunder_agent_loop::loop_engine::engine::ContextInput;

impl From<&Conversation> for ContextInput {
    fn from(conv: &Conversation) -> Self {
        ContextInput::Messages(conv.messages.clone())
    }
}

impl From<Conversation> for ContextInput {
    fn from(conv: Conversation) -> Self {
        ContextInput::Messages(conv.messages)
    }
}

impl Conversation {
    /// Convert conversation messages into a ready-to-run ContextInput for `AgentLoop::run`.
    pub fn as_context_input(&self) -> ContextInput {
        ContextInput::from(self)
    }

    /// Build a fresh ContextBuffer from this conversation's messages.
    pub fn build_context_buffer(&self) -> ContextBuffer {
        ContextBuffer::with_messages(self.messages.clone())
    }
}
