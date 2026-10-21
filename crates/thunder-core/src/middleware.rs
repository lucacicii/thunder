use crate::read_only::{call_is_read_only, COMMAND_KEYS};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use thunder_protocol::{ExecutionContext, ToolExecutionError};

const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "command", "exec", "nohup", "nice", "ionice", "time", "stdbuf", "xargs",
];

/// Verdict returned by a middleware interceptor before tool execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiddlewareVerdict {
    Allow,
    Reject { reason: String },
}

/// Interceptor trait in the Safety & Middleware Pipeline (§1, §2.2).
#[async_trait]
pub trait ToolMiddleware: Send + Sync {
    fn id(&self) -> &str;

    async fn before_call(
        &self,
        tool_name: &str,
        arguments: &Value,
        ctx: &ExecutionContext,
    ) -> MiddlewareVerdict;
}

/// Token-aware kernel security floor blocking destructive shell commands (`rm -rf /`, `mkfs`, `dd if=`, fork bombs)
/// without false positives on quoted strings (e.g. `git commit -m "never run rm -rf /"`).
#[derive(Default)]
pub struct SafetyGuardMiddleware;

#[async_trait]
impl ToolMiddleware for SafetyGuardMiddleware {
    fn id(&self) -> &str {
        "safety-guard"
    }

    async fn before_call(
        &self,
        tool_name: &str,
        arguments: &Value,
        _ctx: &ExecutionContext,
    ) -> MiddlewareVerdict {
        if let Some(cmd) = COMMAND_KEYS
            .iter()
            .find_map(|k| arguments.get(*k).and_then(Value::as_str))
        {
            if let Some((pattern, detail)) = dangerous_command(cmd) {
                return MiddlewareVerdict::Reject {
                    reason: format!(
                        "Blocked destructive command pattern '{pattern}' ({detail}) in tool '{tool_name}'"
                    ),
                };
            }
        }
        MiddlewareVerdict::Allow
    }
}

/// Optional read-only Plan Mode middleware allowing only read-only tools and statically proven read-only shell commands.
#[derive(Default)]
pub struct PlanModeMiddleware;

#[async_trait]
impl ToolMiddleware for PlanModeMiddleware {
    fn id(&self) -> &str {
        "plan-mode"
    }

    async fn before_call(
        &self,
        tool_name: &str,
        arguments: &Value,
        _ctx: &ExecutionContext,
    ) -> MiddlewareVerdict {
        let logical = tool_name
            .strip_prefix("core__")
            .or_else(|| tool_name.strip_prefix("script__"))
            .unwrap_or(tool_name);

        if matches!(
            logical,
            "read_file" | "grep" | "find" | "ls" | "thunder__discover_tools"
        ) {
            return MiddlewareVerdict::Allow;
        }

        if matches!(logical, "bash" | "sh" | "zsh") && call_is_read_only(arguments) {
            return MiddlewareVerdict::Allow;
        }

        MiddlewareVerdict::Reject {
            reason: format!("Plan mode refuses non-read-only tool call '{tool_name}'"),
        }
    }
}

/// Detect unrecoverable shell commands by parsing quoted tokens across command segments.
pub fn dangerous_command(command: &str) -> Option<(&'static str, String)> {
    if let Some(bomb) = fork_bomb(command) {
        return Some(("fork bomb", bomb));
    }
    for words in segments(command) {
        if let Some(hit) = inspect(&words) {
            return Some(hit);
        }
    }
    None
}

fn inspect(words: &[String]) -> Option<(&'static str, String)> {
    let mut verb: Option<&str> = None;
    let mut flags: Vec<&str> = Vec::new();
    let mut operands: Vec<&str> = Vec::new();

    for word in words {
        if verb.is_none() {
            if is_assignment(word)
                || word.starts_with('-')
                || word.chars().all(|c| c.is_ascii_digit())
                || WRAPPERS.contains(&basename(word))
            {
                continue;
            }
            verb = Some(basename(word));
            continue;
        }
        if word.starts_with('-') {
            flags.push(word);
        } else {
            operands.push(word);
        }
    }

    let verb = verb?;

    if verb.starts_with("mkfs") {
        return Some(("mkfs", verb.to_string()));
    }

    if verb == "dd" {
        if let Some(operand) = operands.iter().find(|o| o.starts_with("if=")) {
            return Some(("dd if=", (*operand).to_string()));
        }
        return None;
    }

    if verb == "rm" {
        let recursive = flags
            .iter()
            .any(|f| f.trim_start_matches('-').contains(['r', 'R']));
        let forced = flags.iter().any(|f| f.contains('f'));
        if recursive && forced {
            if let Some(target) = operands.iter().find(|t| is_filesystem_root(t)) {
                return Some(("rm -rf /", (*target).to_string()));
            }
        }
    }

    None
}

fn fork_bomb(command: &str) -> Option<String> {
    let squeezed: String = command.chars().filter(|c| !c.is_whitespace()).collect();
    squeezed
        .contains(":(){:|:&};:")
        .then(|| ":(){ :|:& };:".to_string())
}

fn segments(command: &str) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();

    macro_rules! end_word {
        () => {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        };
    }

    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(escaped) = chars.next() {
                        word.push(escaped);
                    }
                } else {
                    word.push(c);
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        word.push(escaped);
                    }
                }
                ';' | '\n' | '|' | '&' | '(' | ')' | '{' | '}' | '`' | '>' | '<' => {
                    end_word!();
                    if !words.is_empty() {
                        segments.push(std::mem::take(&mut words));
                    }
                }
                c if c.is_whitespace() => end_word!(),
                _ => word.push(c),
            },
        }
    }
    end_word!();
    if !words.is_empty() {
        segments.push(words);
    }
    segments
}

fn is_filesystem_root(target: &str) -> bool {
    let trimmed = target.trim().trim_matches('/');
    trimmed.is_empty() || trimmed == "*"
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Composable middleware pipeline executed for both direct and `thunder__call_tool` invocations.
#[derive(Clone, Default)]
pub struct MiddlewarePipeline {
    stages: Vec<Arc<dyn ToolMiddleware>>,
}

impl MiddlewarePipeline {
    pub fn new() -> Self {
        Self {
            stages: vec![Arc::new(SafetyGuardMiddleware)],
        }
    }

    pub fn with_middleware(mut self, mw: Arc<dyn ToolMiddleware>) -> Self {
        self.stages.push(mw);
        self
    }

    pub async fn check(
        &self,
        tool_name: &str,
        arguments: &Value,
        ctx: &ExecutionContext,
    ) -> Result<(), ToolExecutionError> {
        for stage in &self.stages {
            if let MiddlewareVerdict::Reject { reason } =
                stage.before_call(tool_name, arguments, ctx).await
            {
                return Err(ToolExecutionError::NonZeroExit {
                    code: 403,
                    stderr: format!("[Middleware:{}] {reason}", stage.id()),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_guard_blocks_real_rm_rf_root_but_allows_quoted_mentions() {
        assert_eq!(
            dangerous_command("sudo rm -rf /").map(|(p, _)| p),
            Some("rm -rf /")
        );
        assert_eq!(
            dangerous_command(r#"git commit -m "never run rm -rf /""#),
            None
        );
        assert_eq!(dangerous_command("rm -rf /tmp/build"), None);
    }
}
