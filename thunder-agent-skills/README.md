# ⚡ Thunder Agent Skills

> **High-Performance Skill Parser, Directory Discovery Engine & Execution Registry for Thunder Agent**

`thunder-agent-skills` provides standard skill discovery, parsing, trigger matching, and runtime tool exposure for the Thunder Agent microkernel.

## 🌟 Key Features

1. **Multi-Format Skill Parsing**:
   - `SKILL.md` (YAML Frontmatter + Markdown instructions)
   - Plain Markdown headers (`# SkillName` + `> Description`)
   - Structured JSON & YAML definitions
   - XML tag syntax (`<skill>...<instructions>...</skill>`)
2. **Autonomous Directory Discovery**:
   - Recursive directory scanning (`.agents/skills`, `~/.agents/skills`, `.pi/skills`, etc.)
3. **Intent & Trigger Matching**:
   - Keyword, tag, and intent matching against user prompts.
4. **Dynamic LLM Tools**:
   - `load_skill`: Inspect full skill instructions at runtime.
   - `list_skills`: Browse registered skills and categories.
   - `search_skills`: Semantic keyword matching.

## 🚀 Quick Usage

```rust
use thunder_agent_skills::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = SkillRegistry::new();

    let skill = SkillParser::parse_markdown(
        r#"---
name: code-review
description: Systematic code quality and security review
triggers:
  - review
  - audit
---
## Review Instructions
Check correctness, performance, and security.
"#,
        None,
    )?;

    registry.register(skill).await;

    let matches = registry.match_prompt("Can you review this pull request?").await;
    assert_eq!(matches[0].name, "code-review");

    Ok(())
}
```
