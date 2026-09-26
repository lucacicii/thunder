use serde_json::json;
use std::io::Write;
use tempfile::tempdir;
use thunder_agent_loop::types::tool::{AgentTool, ToolExecutionContext};
use thunder_agent_skills::prelude::*;
use tokio_util::sync::CancellationToken;

#[test]
fn test_parse_markdown_with_yaml_frontmatter() {
    let md = r#"---
name: dividend-cows
description: 筛出 A 股「分红奶牛」
version: 1.2.0
author: lucas
tags:
  - finance
  - stock
triggers:
  - 分红奶牛
  - 连续分红
---
# Instructions
Analyze high dividend yield stocks and payout ratios.
"#;

    let skill = SkillParser::parse_markdown(md, None).expect("Parse should succeed");
    assert_eq!(skill.name, "dividend-cows");
    assert_eq!(skill.description, "筛出 A 股「分红奶牛」");
    assert_eq!(skill.version.as_deref(), Some("1.2.0"));
    assert_eq!(skill.author.as_deref(), Some("lucas"));
    assert_eq!(skill.tags, vec!["finance", "stock"]);
    assert_eq!(skill.triggers, vec!["分红奶牛", "连续分红"]);
    assert!(skill
        .prompt_instructions
        .contains("Analyze high dividend yield stocks"));
}

#[test]
fn test_parse_pure_markdown() {
    let md = r#"# code-review
> Perform systematic code review on Rust projects

Check for unwraps, race conditions, and proper error handling.
"#;

    let skill = SkillParser::parse_markdown(md, None).expect("Parse should succeed");
    assert_eq!(skill.name, "code-review");
    assert_eq!(
        skill.description,
        "Perform systematic code review on Rust projects"
    );
    assert!(skill.prompt_instructions.contains("Check for unwraps"));
}

#[test]
fn test_parse_json_skill() {
    let json_str = r#"{
        "name": "frontend-craft",
        "description": "Craft modern UI with Tailwind CSS",
        "prompt_instructions": "Use responsive utility classes.",
        "tags": ["frontend", "ui"],
        "triggers": ["tailwindcss", "frontend"]
    }"#;

    let skill = SkillParser::parse_json(json_str, None).expect("Parse should succeed");
    assert_eq!(skill.name, "frontend-craft");
    assert_eq!(skill.tags.len(), 2);
}

#[tokio::test]
async fn test_skill_registry_and_matching() {
    let registry = SkillRegistry::new();

    let s1 = Skill::new(
        "git-master",
        "Perform atomic commits and rebase operations",
        "Follow conventional commits.",
    )
    .with_trigger("git")
    .with_trigger("commit")
    .with_tag("vcs");

    let s2 = Skill::new(
        "debugging",
        "Systematic hypothesis-driven debugging",
        "Formulate hypotheses before testing.",
    )
    .with_trigger("debug")
    .with_trigger("error")
    .with_trigger("crash")
    .with_tag("dev");

    registry.register(s1).await;
    registry.register(s2).await;

    assert_eq!(registry.len().await, 2);

    // Test prompt matching
    let matches = registry
        .match_prompt("I have a git rebase conflict on commit")
        .await;
    assert!(!matches.is_empty());
    assert_eq!(matches[0].name, "git-master");

    let matches_debug = registry
        .match_prompt("My program encountered an error and crash")
        .await;
    assert!(!matches_debug.is_empty());
    assert_eq!(matches_debug[0].name, "debugging");
}

#[tokio::test]
async fn test_skill_loader_directory_scan() {
    let dir = tempdir().unwrap();

    let skill1_path = dir.path().join("skill1.md");
    let mut f1 = std::fs::File::create(&skill1_path).unwrap();
    writeln!(
        f1,
        "---\nname: skill-one\ndescription: First test skill\ntriggers: [one]\n---\nBody one"
    )
    .unwrap();

    let sub = dir.path().join("nested");
    std::fs::create_dir(&sub).unwrap();
    let skill2_path = sub.join("skill2.json");
    let mut f2 = std::fs::File::create(&skill2_path).unwrap();
    writeln!(
        f2,
        r#"{{"name":"skill-two","description":"Second skill","prompt_instructions":"Body two","tags":["two"]}}"#
    )
    .unwrap();

    let skills = SkillLoader::load_dir(dir.path())
        .await
        .expect("Dir load should succeed");
    assert_eq!(skills.len(), 2);

    let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"skill-one"));
    assert!(names.contains(&"skill-two"));
}

#[tokio::test]
async fn test_skill_tools_execution() {
    let registry = SkillRegistry::new();
    let skill = Skill::new(
        "playwright-qa",
        "E2E automated browser testing",
        "Launch browser in headless mode and verify locators.",
    )
    .with_tag("testing")
    .with_trigger("playwright");

    registry.register(skill).await;

    let load_tool = LoadSkillTool::new(registry.clone());
    let list_tool = ListSkillsTool::new(registry.clone());
    let search_tool = SearchSkillsTool::new(registry.clone());

    let ctx = ToolExecutionContext {
        tool_call_id: "call_1".to_string(),
        turn: 0,
        cancellation_token: CancellationToken::new(),
    };

    // 1. List skills
    let list_res = list_tool.execute(json!({}), &ctx).await.unwrap();
    assert!(list_res.contains("playwright-qa"));

    // 2. Load skill
    let load_res = load_tool
        .execute(json!({"skill_name": "playwright-qa"}), &ctx)
        .await
        .unwrap();
    assert!(load_res.contains("Launch browser in headless mode"));

    // 3. Search skill
    let search_res = search_tool
        .execute(json!({"query": "browser testing"}), &ctx)
        .await
        .unwrap();
    assert!(search_res.contains("playwright-qa"));
}
