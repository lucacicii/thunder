use thunder_protocol::ChatMessage;

/// Generate 1-3 instant, zero-latency heuristic follow-up suggestions based on conversation context.
pub fn generate_heuristic_suggestions(messages: &[ChatMessage]) -> Vec<String> {
    if messages.is_empty() {
        return vec![
            "分析当前项目的架构与核心模块".to_string(),
            "检查项目的测试与构建门禁状态".to_string(),
        ];
    }

    let mut suggestions = Vec::new();
    let mut has_code_writes = false;
    let mut has_plan_write = false;
    let mut has_plan_exit = false;
    let mut has_doctor = false;
    let mut has_tool_errors = false;
    let mut has_reads_only = false;
    let mut touched_files: Vec<String> = Vec::new();

    // Scan backwards through the last assistant/tool turn
    for msg in messages.iter().rev() {
        match msg {
            ChatMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => {
                for tc in calls {
                    let name = &tc.function.name;
                    let logical = name
                        .strip_prefix("core__")
                        .or_else(|| name.strip_prefix("script__"))
                        .unwrap_or(name);

                    if matches!(logical, "write_file" | "edit_block") {
                        has_code_writes = true;
                        if let Ok(args) =
                            serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                        {
                            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                                if !touched_files.contains(&path.to_string()) {
                                    touched_files.push(path.to_string());
                                }
                            }
                        }
                    } else if logical == "write_plan" {
                        has_plan_write = true;
                    } else if logical == "exit_plan_mode" {
                        has_plan_exit = true;
                    } else if logical == "doctor" {
                        has_doctor = true;
                    } else if matches!(logical, "read_file" | "grep" | "find" | "ls") {
                        has_reads_only = true;
                    }
                }
                break;
            }
            ChatMessage::Tool { content, .. } => {
                if content.contains("[ToolExecutionError")
                    || content.contains("[SystemFault]")
                    || content.contains("error:")
                    || content.contains("Error:")
                {
                    has_tool_errors = true;
                }
            }
            ChatMessage::User { .. }
                if has_code_writes
                    || has_plan_write
                    || has_plan_exit
                    || has_doctor
                    || has_reads_only =>
            {
                break;
            }
            _ => {}
        }
    }

    // High priority 1: Plan mode transitions
    if has_plan_write {
        suggestions.push("退出 Plan 模式并开始执行实施方案".to_string());
        suggestions.push("对方案补充更详细的边界条件与验证步骤".to_string());
        return suggestions;
    }

    if has_plan_exit {
        suggestions.push("开始落地执行方案中的 Step 1".to_string());
        suggestions.push("检查项目当前依赖并准备测试用例".to_string());
        return suggestions;
    }

    // High priority 2: Error recovery
    if has_tool_errors {
        suggestions.push("分析刚才的错误堆栈并尝试自动修复".to_string());
        suggestions.push("使用更详细的命令重新诊断失败原因".to_string());
        return suggestions;
    }

    // High priority 3: Code mutations
    if has_code_writes {
        if let Some(first_file) = touched_files.first() {
            suggestions.push(format!("为刚才修改的 `{first_file}` 编写配套测试用例"));
        } else {
            suggestions.push("为刚才修改的代码编写配套单元测试".to_string());
        }
        suggestions.push("查看刚才修改引起的 git diff 并检查语法".to_string());
        suggestions.push("运行项目的测试门禁验证本次改动".to_string());
        suggestions.truncate(3);
        return suggestions;
    }

    // High priority 4: Diagnostic tool
    if has_doctor {
        suggestions.push("根据诊断报告修复缺失的依赖与配置".to_string());
        return suggestions;
    }

    // High priority 5: Read-only search
    if has_reads_only {
        suggestions.push("基于检索到的代码逻辑编写落地代码".to_string());
        suggestions.push("进一步搜索相关调用方与依赖模块".to_string());
        return suggestions;
    }

    // Default conversational fallbacks
    vec![
        "运行完整构建与单元测试门禁".to_string(),
        "总结本次修改并准备 git commit".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use thunder_protocol::ToolCall;

    #[test]
    fn suggests_test_and_diff_after_code_write() {
        let msgs = vec![
            ChatMessage::user("Modify foo"),
            ChatMessage::assistant(
                Some("Writing file".into()),
                Some(vec![ToolCall::new_function(
                    "t1",
                    "core__write_file",
                    r#"{"path":"src/foo.rs","content":"fn foo() {}"}"#,
                )]),
            ),
        ];

        let suggestions = generate_heuristic_suggestions(&msgs);
        assert!(!suggestions.is_empty());
        assert!(suggestions[0].contains("src/foo.rs"));
        assert!(suggestions.iter().any(|s| s.contains("git diff")));
    }

    #[test]
    fn suggests_plan_exit_after_write_plan() {
        let msgs = vec![
            ChatMessage::user("Plan feature"),
            ChatMessage::assistant(
                Some("Plan written".into()),
                Some(vec![ToolCall::new_function(
                    "t1",
                    "core__write_plan",
                    r##"{"content":"# Implementation Plan"}"##,
                )]),
            ),
        ];

        let suggestions = generate_heuristic_suggestions(&msgs);
        assert!(suggestions.iter().any(|s| s.contains("退出 Plan 模式")));
    }
}
