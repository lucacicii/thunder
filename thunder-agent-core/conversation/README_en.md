# ⚡ Thunder Conversation

> **High-Performance, Orchestration-Aware Conversation & Session Management**

[English](README_en.md) | [简体中文](README.md)

`thunder-conversation` is the core session management sub-package in the Thunder Agent ecosystem. It manages conversation lifecycles, Turn groupings, context bridging, and hierarchical persistence for single and multi-agent workflows.

---

## 🌟 Key Features

- **Orchestration Topology Awareness**:
  - **Sequential Pipelines**: Stage records (`StageRecord`) tracking planner → coder handoff artifacts.
  - **Parallel Branching**: Session branching (`create_branch`) and multi-perspective synthesis (`merge_parallel_into_parent`).
  - **Delegated Tasks**: Tool-level encapsulation (`record_delegated_task`) with drill-down sub-conversations.
- **Dual-Engine Storage**:
  - **`MemoryConversationStore`**: Thread-safe in-memory store (`RwLock<HashMap>`) for tests, short-lived tasks, and fast caching.
  - **`FsConversationStore`**: Atomic file-based JSON persistence (`rename`) with `index.json` caching for fast listing without reading every payload.
- **Turn Abstraction & Pruning**:
  - Automatically groups `User -> Assistant -> Tool -> Assistant` into logical Turns.
  - Supports turn-based truncation (`truncate_turns`) preserving System Prompt.
- **Seamless Engine Interoperability**:
  - Convert directly to `thunder-agent-loop`'s `ContextInput` / `ContextBuffer`.
- **Rich Formatted Exports**:
  - Export to structured Markdown, standard JSON, and OpenAI-compatible message arrays.

---

## 🚀 Quickstart

```rust
use thunder_conversation::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize file-system store
    let store = FsConversationStore::new("~/.thunder/conversations").await?;
    let manager = ConversationManager::new(store);

    // 2. Create conversation
    let mut conv = manager
        .create_with_prompt(
            "session_001",
            Some("System Architecture Discussion".to_string()),
            Some("You are a senior system architect.".to_string()),
        )
        .await?;

    // 3. Append user message
    conv.add_user_message("How to design a high-throughput Agent loop engine?");
    manager.save(&conv).await?;

    // 4. Convert to Agent loop input
    let input = conv.as_context_input();

    // 5. Export formatted Markdown
    let md = ConversationExporter::to_markdown(&conv);
    println!("{md}");

    Ok(())
}
```

---

## 🧪 Testing

```bash
# Run all conversation unit and integration tests
./test.sh
```
