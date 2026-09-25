# ⚡ Thunder Agent Skills

> **Thunder Agent 高性能技能解析、目录发现引擎与全局执行注册表**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-skills` 负责技能（Skill）的标准化发现、多格式解析、意图触发词匹配以及运行时动态工具暴露，是 Thunder 微内核宿主常驻的核心基线能力之一。

---

## 🚀 核心特性

1. **多格式规范解析**：
   - `SKILL.md`（YAML Frontmatter 元数据 + Markdown 指令正文）。
   - 纯 Markdown 标题格式（`# 技能名` + `> 描述`）。
   - 结构化 JSON 与 YAML 配置文件。
   - XML 标签语法（`<skill>...<instructions>...</skill>`）。
2. **自动化目录扫描与全局缓存（120s TTL）**：
   - 递归扫描工程工作区（`.agents/skills`、`.pi/skills`）与用户全局目录（`~/.agents/skills`）。
   - 内置 `DEFAULT_SKILLS_CACHE` 全局缓存，TTL 设为 120 秒，避免多轮高频对话下反复遍历文件系统造成的 I/O 阻塞与性能毛刺。
3. **意图触发词快速匹配**：
   - 对用户输入的 Prompt 进行毫秒级关键词、标签与意图匹配，精准筛选与任务相关的 Playbook。
4. **动态运行时工具集成**：
   - `load_skill`：LLM 在需要时按需调用，调阅完整的技能操作指南。
   - `list_skills`：列出当前已注册的所有技能目录与简述。
   - `search_skills`：基于关键字快速检索匹配的技能。

---

## 🛠️ 快速上手

```rust
use thunder_agent_skills::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = SkillRegistry::new();

    // 1. 解析 Markdown 技能
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

    // 2. 注册进注册表
    registry.register(skill).await;

    // 3. 意图匹配
    let matches = registry.match_prompt("Can you review this pull request?").await;
    assert_eq!(matches[0].name, "code-review");

    Ok(())
}
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
