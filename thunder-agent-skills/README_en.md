# ⚡ Thunder Agent Skills

> **High-Performance Skill Parser, Directory Discovery Engine & Execution Registry for Thunder Agent**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-skills` provides standard skill discovery, multi-format parsing, intent trigger matching, and runtime tool exposure for the Thunder Agent microkernel.

---

## 🚀 Key Features

1. **Multi-Format Skill Parsing**:
   - `SKILL.md` (YAML Frontmatter metadata + Markdown instruction body).
   - Plain Markdown headers (`# SkillName` + `> Description`).
   - Structured JSON & YAML configuration definitions.
   - XML tag syntax (`<skill>...<instructions>...</skill>`).
2. **Autonomous Directory Discovery & Global Caching (120s TTL)**:
   - Recursively scans project directories (`.agents/skills`, `.pi/skills`) and user global directories (`~/.agents/skills`).
   - Integrated with `DEFAULT_SKILLS_CACHE` enforcing a 120-second TTL, eliminating redundant disk walks and I/O bottlenecks during high-frequency turns.
3. **Instant Intent & Trigger Matching**:
   - Matches keywords, tags, and semantic intent against user prompts in sub-milliseconds to activate relevant playbooks.
4. **Dynamic LLM Tools**:
   - `load_skill`: Fetches and inspects full playbook instructions dynamically.
   - `list_skills`: Browses all registered skills, summaries, and categories.
   - `search_skills`: Searches available skills by keyword.

---

## 🛠️ Quickstart

```rust
use thunder_agent_skills::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = SkillRegistry::new();

    // 1. Parse markdown skill
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

    // 2. Register into registry
    registry.register(skill).await;

    // 3. Match against user prompt
    let matches = registry.match_prompt("Can you review this pull request?").await;
    assert_eq!(matches[0].name, "code-review");

    Ok(())
}
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](../LICENSE).
