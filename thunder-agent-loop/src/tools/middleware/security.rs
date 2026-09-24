use crate::tools::middleware::telemetry::SystemNotice;
use crate::tools::middleware::{ToolHandler, ToolMiddleware};
use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Security Guard Middleware.
///
/// Enforces workspace jail boundaries (preventing path traversal attacks like `../../etc/passwd`),
/// and blocks dangerous/destructive shell commands before execution.
///
/// The jail is multi-root: the primary workspace plus any extra roots the host
/// granted (e.g. repositories referenced by the task). All roots share the same
/// read/write standing; relative paths always resolve against the primary root.
///
/// Shell command inspection is best-effort, not a shell interpreter: it blocks
/// high-confidence violations (destructive patterns, statically-known absolute
/// write targets outside the roots). Targets containing substitutions (`$`,
/// backticks, `~`) cannot be resolved here and are skipped.
#[derive(Clone)]
pub struct SecurityGuardMiddleware {
    /// Primary workspace root: relative paths resolve here (always allowed_roots[0]).
    workspace_root: PathBuf,
    /// Primary root + extra roots, canonicalized. A path is jailed in iff it is
    /// contained in at least one of these.
    allowed_roots: Vec<PathBuf>,
    forbidden_commands: Vec<String>,
}

/// Special device targets that redirects may legitimately point at.
const DEVICE_ALLOWLIST: &[&str] = &["/dev/null", "/dev/stdin", "/dev/stdout", "/dev/stderr"];

/// Commands whose every non-option operand is a write target.
const WRITE_ALL_OPERANDS: &[&str] = &[
    "rm", "tee", "mkdir", "touch", "truncate", "unlink", "rmdir", "shred",
];
/// Commands whose LAST non-option operand is the write target (sources precede it).
const WRITE_LAST_OPERAND: &[&str] = &["cp", "mv", "ln", "install"];
/// Commands that take one control operand first (mode / owner), then targets.
const WRITE_AFTER_FIRST_OPERAND: &[&str] = &["chmod", "chown"];
/// Command separators that reset per-command parsing state.
const SEPARATORS: &[&str] = &["|", "||", "&&", ";", ";;", "&"];

impl SecurityGuardMiddleware {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let root: PathBuf = workspace_root.into();
        let ws = normalize_path(&root.canonicalize().unwrap_or(root));
        Self {
            workspace_root: ws.clone(),
            allowed_roots: vec![ws],
            forbidden_commands: vec![
                "rm -rf /".to_string(),
                "rm -rf /*".to_string(),
                ":(){ :|:& };:".to_string(),
                "mkfs".to_string(),
                "dd if=".to_string(),
            ],
        }
    }

    /// Grant extra roots (e.g. referenced repositories) the same read/write
    /// standing as the primary workspace.
    pub fn with_extra_roots(
        mut self,
        roots: impl IntoIterator<Item = impl Into<PathBuf>>,
    ) -> Self {
        for root in roots {
            let raw: PathBuf = root.into();
            let canonical = normalize_path(&raw.canonicalize().unwrap_or(raw));
            if !self.allowed_roots.contains(&canonical) {
                self.allowed_roots.push(canonical);
            }
        }
        self
    }

    pub fn with_forbidden_command(mut self, cmd: impl Into<String>) -> Self {
        self.forbidden_commands.push(cmd.into());
        self
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// All roots a path may live in (primary first).
    pub fn allowed_roots(&self) -> &[PathBuf] {
        &self.allowed_roots
    }

    fn roots_display(&self) -> String {
        self.allowed_roots
            .iter()
            .map(|r| r.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Whether a normalized absolute path is jailed in (contained in any root).
    fn is_allowed(&self, normalized: &Path) -> bool {
        self.allowed_roots.iter().any(|root| normalized.starts_with(root))
    }

    /// Normalizes and validates whether a target path is within the multi-root jail.
    pub fn check_path(&self, target: &Path) -> Result<PathBuf, String> {
        let candidate = if target.is_relative() {
            self.workspace_root.join(target)
        } else {
            target.to_path_buf()
        };

        // Resolve symlinks (e.g. macOS /var → /private/var) so legitimately
        // in-root paths are not lexically rejected, and symlink escapes out of
        // the jail are caught. Non-existent tails resolve via the deepest
        // existing ancestor.
        let normalized = normalize_path(&resolve_for_check(&candidate));

        if !self.is_allowed(&normalized) {
            return Err(format!(
                "Path traversal detected! Path '{}' escapes all allowed workspace roots [{}]",
                target.display(),
                self.roots_display()
            ));
        }

        Ok(normalized)
    }

    /// Validates a statically-known absolute shell target. Unresolvable targets
    /// (containing `$`, backticks, `~`) and non-absolute targets pass through.
    fn check_static_target(&self, target: &str) -> Result<(), String> {
        let target = target.trim_matches(|c| c == '"' || c == '\'');
        if !target.starts_with('/') {
            return Ok(());
        }
        if target.contains('$') || target.contains('`') || target.contains('~') {
            return Ok(());
        }
        if DEVICE_ALLOWLIST.iter().any(|dev| target == *dev || target.starts_with(&format!("{}/fd/", dev))) {
            return Ok(());
        }
        let normalized = normalize_path(&resolve_for_check(Path::new(target)));
        if self.is_allowed(&normalized) {
            Ok(())
        } else {
            Err(format!(
                "Shell write target '{}' escapes all allowed workspace roots [{}]",
                target,
                self.roots_display()
            ))
        }
    }

    /// Checks a bash command for forbidden destructive patterns and write
    /// targets outside the multi-root jail.
    pub fn check_command(&self, command: &str) -> Result<(), String> {
        let trimmed = command.trim();
        for forbidden in &self.forbidden_commands {
            if trimmed.contains(forbidden) {
                return Err(format!("Forbidden high-risk command pattern detected: '{}'", forbidden));
            }
        }
        for target in extract_write_targets(trimmed) {
            if let Err(violation) = self.check_static_target(&target) {
                return Err(violation);
            }
        }
        Ok(())
    }
}

/// Extracts candidate write-target strings from a shell command line:
/// redirection operands (`>`, `>>`, `2>`) and operands of write-type commands
/// (`rm`, `cp`, `tee`, `sed -i`, ...). Best-effort tokenization on whitespace;
/// callers decide what to enforce.
fn extract_write_targets(command: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut i = 0;

    // Active write-command mode across tokens until a separator resets it.
    enum WriteMode {
        None,
        AllOperands,
        LastOperand(Vec<String>),
        AfterFirst(Option<String>),
    }
    let mut mode = WriteMode::None;
    // Whether the current token sits in command position (segment start),
    // so write-command keywords are only recognized as verbs, never as
    // operands of an unrelated command (e.g. `echo cp /etc/x`).
    let mut command_position = true;

    while i < tokens.len() {
        let raw = tokens[i];
        let tok = raw.trim_end_matches(|c| c == ';' || c == ',');
        let bare = tok.trim_matches(|c| c == '"' || c == '\'');

        if SEPARATORS.contains(&bare) || bare == ";" {
            mode = WriteMode::None;
            command_position = true;
            i += 1;
            continue;
        }

        // 1) Redirection: `>`, `>>`, `2>`, `2>>`, `&>`, possibly fused (`>/x`).
        if let Some(embedded) = redirect_operand(bare) {
            match embedded {
                Some(target) if !target.is_empty() => targets.push(target.to_string()),
                _ => {
                    if let Some(next) = tokens.get(i + 1) {
                        targets.push(next.trim_matches(|c| c == '"' || c == '\'').to_string());
                        i += 1;
                    }
                }
            }
            command_position = false;
            i += 1;
            continue;
        }

        // 2) Command words are only recognized in command position.
        if command_position {
            mode = if WRITE_ALL_OPERANDS.contains(&bare) {
                WriteMode::AllOperands
            } else if WRITE_LAST_OPERAND.contains(&bare) {
                WriteMode::LastOperand(Vec::new())
            } else if WRITE_AFTER_FIRST_OPERAND.contains(&bare) {
                WriteMode::AfterFirst(None)
            } else if bare == "sed" {
                // `sed -i` rewrites its file operands; check every absolute
                // path-looking token in the command conservatively.
                let in_place = tokens
                    .iter()
                    .any(|t| t.starts_with("-i") || t.starts_with("--in-place"));
                if in_place {
                    for t in &tokens {
                        let t = t.trim_matches(|c| c == '"' || c == '\'');
                        if t.starts_with('/') {
                            targets.push(t.to_string());
                        }
                    }
                }
                WriteMode::None
            } else {
                // Unrelated command (echo, ls, grep, ...): its operands are not
                // write targets for this best-effort scan.
                WriteMode::None
            };
            command_position = false;
            i += 1;
            continue;
        }

        // 3) Collect operands for the active write-command mode.
        if tok.starts_with('-') && tok.len() > 1 {
            // Option token (e.g. `-r`, `-i.bak`) — never an operand.
            i += 1;
            continue;
        }
        match &mut mode {
            WriteMode::AllOperands => targets.push(bare.to_string()),
            WriteMode::LastOperand(operands) => operands.push(bare.to_string()),
            WriteMode::AfterFirst(control) => {
                if control.is_none() {
                    // First operand is the control operand (mode / owner spec).
                    *control = Some(bare.to_string());
                } else {
                    targets.push(bare.to_string());
                }
            }
            WriteMode::None => {}
        }
        i += 1;
    }

    if let WriteMode::LastOperand(operands) = mode {
        if let Some(last) = operands.last() {
            targets.push(last.clone());
        }
    }
    targets
}

/// For a token that is (or starts with) a redirection operator:
/// `Some(Some(target))` — operator fused with its target (`>/x`, `2>>/x`);
/// `Some(None)` — bare operator (`>`, `2>`), target is the next token;
/// `None` — not a redirection.
fn redirect_operand(token: &str) -> Option<Option<&str>> {
    let mut rest = token;
    // Strip an optional leading fd number or `&` (`2>`, `&>>`).
    if let Some(stripped) = rest.strip_prefix('&') {
        rest = stripped;
    } else {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && digits < rest.len() {
            rest = &rest[digits..];
        }
    }
    if let Some(target) = rest.strip_prefix(">>") {
        Some(Some(target))
    } else if let Some(target) = rest.strip_prefix('>') {
        Some(Some(target))
    } else {
        None
    }
}

/// Resolves a path to its true filesystem location: symlinks expanded via
/// `canonicalize` on the deepest existing ancestor, with any non-existent
/// tail re-appended. Paths that cannot be resolved at all pass through as-is.
fn resolve_for_check(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while let Some(parent) = existing.parent() {
        if parent == existing {
            break;
        }
        match existing.canonicalize() {
            Ok(canonical) => {
                let mut resolved = canonical;
                for segment in tail.iter().rev() {
                    resolved.push(segment);
                }
                return resolved;
            }
            Err(_) => {
                if let Some(name) = existing.file_name() {
                    tail.push(name.to_os_string());
                }
                existing = parent.to_path_buf();
            }
        }
    }
    path.to_path_buf()
}

/// Normalizes a path by resolving `.` and `..` components logically.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => out.push(Component::Prefix(p)),
            Component::RootDir => out.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(c) => out.push(c),
        }
    }
    out
}

#[async_trait]
impl ToolMiddleware for SecurityGuardMiddleware {
    fn name(&self) -> &str {
        "SecurityGuardMiddleware"
    }

    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult {
        let start = Instant::now();

        // 1. Inspect arguments for path traversal
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.function.arguments) {
            // Check 'path' parameter
            if let Some(path_str) = args.get("path").and_then(|v| v.as_str()) {
                if let Err(violation) = self.check_path(Path::new(path_str)) {
                    let notice = SystemNotice::new(
                        "SecurityGuard",
                        "Path traversal attack blocked",
                        "No file system modifications were performed. Command was halted at security perimeter.",
                    )
                    .with_guidance(format!(
                        "Confine all file operations within the allowed workspace roots: [{}]. \
                         Paths inside any listed root are read/write accessible.",
                        self.roots_display()
                    ));

                    return ToolExecutionResult::error(format!("Access Denied: {}", violation), start.elapsed())
                        .with_telemetry(notice);
                }
            }

            // Check 'cwd' parameter
            if let Some(cwd_str) = args.get("cwd").and_then(|v| v.as_str()) {
                if let Err(violation) = self.check_path(Path::new(cwd_str)) {
                    let notice = SystemNotice::new(
                        "SecurityGuard",
                        "Working directory traversal blocked",
                        "Process spawn aborted. Cwd escaped sandbox roots.",
                    )
                    .with_guidance(format!(
                        "Ensure working directory targets remain inside the allowed roots: [{}].",
                        self.roots_display()
                    ));

                    return ToolExecutionResult::error(format!("Access Denied: {}", violation), start.elapsed())
                        .with_telemetry(notice);
                }
            }

            // Check bash commands for high-risk patterns and out-of-jail writes
            if call.function.name == "bash" {
                if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                    if let Err(violation) = self.check_command(cmd) {
                        let is_write_escape = violation.starts_with("Shell write target");
                        let (action, ground_truth, guidance): (&str, &str, String) = if is_write_escape {
                            (
                                "Out-of-jail shell write blocked",
                                "No file system modifications were performed. Command was halted at security perimeter.",
                                format!(
                                    "Use write_file for edits, or keep shell write targets inside the allowed workspace roots: [{}].",
                                    self.roots_display()
                                ),
                            )
                        } else {
                            (
                                "Forbidden destructive command blocked",
                                "Command was intercepted before being dispatched to the shell subsystem.",
                                "Dangerous destructive shell operations are disabled by safety guardrails.".to_string(),
                            )
                        };

                        let notice = SystemNotice::new("SecurityGuard", action, ground_truth)
                            .with_guidance(guidance);

                        return ToolExecutionResult::error(format!("Execution Blocked: {}", violation), start.elapsed())
                            .with_telemetry(notice);
                    }
                }
            }
        }

        // Passed security perimeter, forward to next layer
        next.handle(call, ctx, timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    struct DummyNext;
    #[async_trait]
    impl ToolHandler for DummyNext {
        async fn handle(
            &self,
            _call: &ToolCall,
            _ctx: &ToolExecutionContext,
            _timeout: Option<Duration>,
        ) -> ToolExecutionResult {
            ToolExecutionResult::success("allowed".to_string(), Duration::from_millis(1))
        }
    }

    fn ctx(id: &str) -> ToolExecutionContext {
        ToolExecutionContext {
            tool_call_id: id.to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
        }
    }

    fn bash_call(id: &str, command: &str) -> ToolCall {
        ToolCall::new_function(
            id,
            "bash",
            serde_json::json!({ "command": command }).to_string(),
        )
    }

    fn write_call(id: &str, path: &str) -> ToolCall {
        ToolCall::new_function(
            id,
            "write_file",
            serde_json::json!({ "path": path, "content": "x" }).to_string(),
        )
    }

    async fn run(guard: &SecurityGuardMiddleware, call: &ToolCall, id: &str) -> ToolExecutionResult {
        guard.handle(call, &ctx(id), None, Arc::new(DummyNext)).await
    }

    #[tokio::test]
    async fn test_path_traversal_blocked_with_telemetry() {
        let ws = std::env::temp_dir().join("thunder_sec_test");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        let call = write_call("call_sec_1", "../../etc/shadow");

        let res = run(&guard, &call, "call_sec_1").await;
        assert!(res.is_error);
        assert!(res.output.contains("Path traversal detected"));
        assert!(res.output.contains("[System Telemetry: SecurityGuard"));
        assert!(res.output.contains("Path traversal attack blocked"));
        // The rejection names every allowed root so the model knows legal targets.
        assert!(res.output.contains(ws.canonicalize().unwrap().to_string_lossy().as_ref()));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_forbidden_command_blocked() {
        let ws = std::env::temp_dir().join("thunder_sec_test_cmd");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        let res = run(&guard, &bash_call("call_sec_2", "sudo rm -rf /"), "call_sec_2").await;
        assert!(res.is_error);
        assert!(res.output.contains("Forbidden high-risk command pattern"));
        assert!(res.output.contains("Dangerous destructive shell operations are disabled"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_valid_path_allowed() {
        let ws = std::env::temp_dir().join("thunder_sec_test_ok");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        let res = run(&guard, &write_call("call_sec_3", "src/main.rs"), "call_sec_3").await;
        assert!(!res.is_error);
        assert_eq!(res.output, "allowed");

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_extra_root_grants_access() {
        let ws = std::env::temp_dir().join("thunder_sec_multi_ws");
        let repo = std::env::temp_dir().join("thunder_sec_multi_repo");
        let _ = std::fs::create_dir_all(&ws);
        let _ = std::fs::create_dir_all(&repo);
        let guard = SecurityGuardMiddleware::new(&ws).with_extra_roots([&repo]);

        // Path inside the extra root passes for both read-style and write calls.
        let target = repo.join("src/main.rs");
        let res = run(&guard, &write_call("call_m1", target.to_str().unwrap()), "call_m1").await;
        assert!(!res.is_error, "extra root path must pass");

        // Relative path still resolves against the primary root.
        let res = run(&guard, &write_call("call_m2", "notes.txt"), "call_m2").await;
        assert!(!res.is_error);

        // Outside all roots still blocked, and both roots are named.
        let res = run(&guard, &write_call("call_m3", "/etc/shadow"), "call_m3").await;
        assert!(res.is_error);
        assert!(res.output.contains("escapes all allowed workspace roots"));
        assert!(res.output.contains(repo.canonicalize().unwrap().to_string_lossy().as_ref()));

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn test_bash_write_escape_blocked() {
        let ws = std::env::temp_dir().join("thunder_sec_bash_ws");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        for cmd in [
            "echo hacked > /etc/hosts",
            "echo hacked >> /etc/hosts",
            "echo hacked 2> /etc/hosts",
            "echo hacked >/etc/hosts",
            "tee /etc/hosts",
            "sed -i 's/a/b/' /etc/hosts",
            "cp inner.txt /etc/hosts",
            "mv inner.txt /etc/hosts",
            "rm /etc/hosts",
            "chmod 644 /etc/hosts",
        ] {
            let res = run(&guard, &bash_call("call_b1", cmd), "call_b1").await;
            assert!(res.is_error, "must block: {cmd}");
            assert!(
                res.output.contains("Shell write target") || res.output.contains("Forbidden"),
                "violation reason for '{cmd}': {}",
                res.output
            );
        }

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_bash_benign_and_in_jail_commands_allowed() {
        let ws = std::env::temp_dir().join("thunder_sec_bash_ok");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        let in_jail = ws.join("out.txt");
        for cmd in [
            "ls /etc | head -5",
            "echo /etc/passwd is readable text",
            "echo data > out.txt",
            format!("echo data > {}", in_jail.display()).as_str(),
            "echo x > /dev/null",
            "grep -r foo . > result.log",
        ] {
            let res = run(&guard, &bash_call("call_b2", cmd), "call_b2").await;
            assert!(!res.is_error, "must allow: {cmd} ({})", res.output);
        }

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn test_extract_write_targets_shapes() {
        let targets = extract_write_targets("echo a > /tmp/x && cp /tmp/x /etc/y");
        assert!(targets.contains(&"/tmp/x".to_string()));
        assert!(targets.contains(&"/etc/y".to_string()));

        // Last-operand semantics: only the destination of cp is a target.
        let targets = extract_write_targets("cp /a/b c.txt");
        assert_eq!(targets, vec!["c.txt".to_string()]);

        // Write-command keywords are only verbs, never echo operands.
        let targets = extract_write_targets("echo cp /etc/x");
        assert!(targets.is_empty(), "echo operands must not be targets: {targets:?}");

        // chmod: mode operand is skipped, targets follow.
        let targets = extract_write_targets("chmod 644 /etc/hosts /etc/passwd");
        assert_eq!(
            targets,
            vec!["/etc/hosts".to_string(), "/etc/passwd".to_string()]
        );

        // Option tokens never count as operands.
        let targets = extract_write_targets("rm -rf subdir");
        assert_eq!(targets, vec!["subdir".to_string()]);
    }
}
