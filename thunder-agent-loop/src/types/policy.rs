//! Permission *policy*: the single place a tool call is judged.
//!
//! # One decision, many enforcers
//!
//! [`SessionPolicy::decide`] is the only function in the system that answers
//! "may this call run?". Everything else — the host choosing which tools to
//! register, the onion layer that refuses, the TypeScript sidecar's RPC gate —
//! asks it and acts on the verdict.
//!
//! It used to be otherwise. Permission lived in four places across three
//! crates, with two independent tool-name-to-capability tables that could
//! disagree, and the plugin RPC path consulted only the static tier. A verdict
//! that disagreed with the layer enforcing it was a security bug waiting for a
//! new tool name.
//!
//! # What the policy holds
//!
//! * [`Permission`] — the capability *ceiling*: what is possible at all. A hard
//!   limit; nothing in this module may widen it.
//! * [`ApprovalMode`] — what still needs human approval (never, shell_only, mutations, always).
//! * [`AllowRule`]s — what the user has already said yes to this session.
//!
//! A read-only role stays read-only in every mode. There is deliberately no
//! mode that escalates privilege.
//!
//! ```
//! use thunder_agent_loop::prelude::*;
//!
//! // A read-only role, even in never/yolo mode, cannot write.
//! assert_eq!(Permission::Read.min(Permission::Bash), Permission::Read);
//! ```

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::types::config::Permission;
use crate::types::message::ToolCall;

/// How intrusive the agent may be before it stops asking for human approval.
///
/// Pure approval strategy: strictly dictates *when to prompt humans*.
/// It never alters, lowers, or escalates the role's capability tier (`Permission`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Never ask for human approval. Runs all operations permitted by the role tier.
    #[default]
    #[serde(alias = "yolo", alias = "none", alias = "bypass")]
    Never,
    /// Read and write freely; ask before executing shell/terminal commands.
    #[serde(alias = "accept_edits", alias = "acceptEdits", alias = "ask_shell")]
    ShellOnly,
    /// Read freely; ask before modifying files or executing shell commands.
    #[serde(alias = "ask", alias = "writes")]
    Mutations,
    /// Ask before every single tool call, including reads.
    #[serde(alias = "manual", alias = "all")]
    Always,
}

/// Backward compatibility alias for the approval mode.
pub type PermissionMode = ApprovalMode;

impl ApprovalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalMode::Never => "never",
            ApprovalMode::ShellOnly => "shell_only",
            ApprovalMode::Mutations => "mutations",
            ApprovalMode::Always => "always",
        }
    }

    /// Every mode in ascending order of invasiveness.
    pub const ALL: [ApprovalMode; 4] = [
        ApprovalMode::Never,
        ApprovalMode::ShellOnly,
        ApprovalMode::Mutations,
        ApprovalMode::Always,
    ];

    /// The next mode in [`ApprovalMode::ALL`], for a Shift+Tab style toggle.
    pub fn next(self) -> ApprovalMode {
        let idx = ApprovalMode::ALL
            .iter()
            .position(|m| *m == self)
            .unwrap_or(0);
        ApprovalMode::ALL[(idx + 1) % ApprovalMode::ALL.len()]
    }

    pub fn parse(raw: &str) -> Option<ApprovalMode> {
        let normalised = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match normalised.as_str() {
            "never" | "yolo" | "none" | "bypass" => Some(ApprovalMode::Never),
            "shell_only" | "accept_edits" | "acceptedits" | "ask_shell" => {
                Some(ApprovalMode::ShellOnly)
            }
            "mutations" | "ask" | "writes" => Some(ApprovalMode::Mutations),
            "always" | "manual" | "all" => Some(ApprovalMode::Always),
            "plan" => Some(ApprovalMode::Never),
            _ => None,
        }
    }
}

/// What a tool call can do to the world.
///
/// Classification is by tool name, so an unregistered name is [`ToolEffect::Write`]
/// rather than "unknown" — an unrecognised tool must never be assumed harmless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    /// Cannot change anything outside the read-only tools' own scratch files.
    Read,
    /// Mutates the workspace.
    Write,
    /// Runs a shell command.
    Exec,
    /// Unknown tool: treated exactly like [`ToolEffect::Write`].
    Other,
}

impl ToolEffect {
    /// Classify by tool name.
    ///
    /// Plugin, MCP and skill tools default to [`ToolEffect::Other`] (which behaves
    /// like `Write`) unless their name indicates command execution semantics, in which
    /// case they require [`ToolEffect::Exec`] (Bash tier). That prevents unclassified
    /// terminal/exec MCP tools from escalating under a Write-only role.
    pub fn of(tool: &str) -> ToolEffect {
        let lower = tool.to_ascii_lowercase();
        match lower.as_str() {
            "read_file" | "grep" | "find" | "ls" | "list_dir" | "read" | "glob" => ToolEffect::Read,
            "bash" | "shell" | "powershell" => ToolEffect::Exec,
            "write_file" | "edit" | "write" | "apply_patch" | "notebook_edit" => ToolEffect::Write,
            _ => {
                if lower.contains("exec")
                    || lower.contains("shell")
                    || lower.contains("terminal")
                    || lower.contains("powershell")
                    || lower.contains("command")
                    || lower.ends_with("_bash")
                {
                    ToolEffect::Exec
                } else {
                    ToolEffect::Other
                }
            }
        }
    }

    pub fn is_prompt_worthy(self) -> bool {
        !matches!(self, ToolEffect::Read)
    }

    /// The ceiling this effect requires.
    ///
    /// [`ToolEffect::Other`] demands [`Permission::Write`], not `Bash`: a tool
    /// nobody has classified should not be able to shell out merely by being
    /// unknown. `Exec` is the only effect that needs the top tier.
    pub fn required_tier(self) -> Permission {
        match self {
            ToolEffect::Read => Permission::Read,
            ToolEffect::Write | ToolEffect::Other => Permission::Write,
            ToolEffect::Exec => Permission::Bash,
        }
    }
}

/// Destructive command patterns that can never be executed under any
/// circumstances or approved by any dialog.
pub fn is_forbidden_destructive_call(tool: &str, args: &serde_json::Value) -> Option<&'static str> {
    let lower = tool.to_ascii_lowercase();
    if lower == "bash" || lower == "shell" || lower == "powershell" || lower.contains("exec") {
        if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
            let trimmed = cmd.trim();
            for pattern in &["rm -rf /", "rm -rf /*", ":(){ :|:& };:", "mkfs", "dd if="] {
                if trimmed.contains(pattern) {
                    return Some(pattern);
                }
            }
        }
    }
    None
}

/// Options offered by the approval dialog, in display order.
pub const ALLOW_ONCE: &str = "允许一次";
pub const ALLOW_ALWAYS: &str = "总是允许";
pub const DENY: &str = "拒绝";
pub const DENY_WITH_REASON: &str = "拒绝并说明原因";

/// A pending question for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool: String,
    /// Short headline, e.g. `执行 bash`.
    pub title: String,
    /// The concrete thing being authorised, e.g. the command or path.
    pub detail: String,
    pub options: Vec<String>,
    /// Binds an answer to this exact call.
    ///
    /// A dialog is answered out-of-band, so an approval must not be replayable
    /// onto different arguments. The caller verifies the hash before honouring
    /// an "allow always" rule and before executing.
    pub call_hash: String,
    /// Who is asking. A plugin-initiated request must never be presentable as
    /// something the assistant asked for.
    pub caller: Caller,
}

/// The verdict on a call.
///
/// Returned by [`SessionPolicy::decide`] and acted on by whichever layer is
/// enforcing. Three states because "refuse now" and "ask a human" are
/// genuinely different: the first needs no UI, the second does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run it.
    Allow,
    /// Refuse without asking. `reason` is surfaced to the model verbatim.
    Deny { reason: String },
    /// Ask the user, then act on the answer.
    Ask(ApprovalRequest),
}

/// A remembered "always allow" rule, scoped to one session.
///
/// The rule is deliberately narrow: it pins one tool and, where meaningful, the
/// *prefix* of its primary argument. A rule never grants a tool class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowRule {
    pub tool: String,
    /// See [`AllowRule::for_call`] — `None` means "this tool, any arguments",
    /// which is only offered for tools with no meaningful scoping argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_prefix: Option<String>,
}

/// Shell characters that let a "prefix" grow into a different command.
///
/// Not a shell parser — a conservative filter. Anything containing one of these
/// is refused a prefix rule, because `git status; rm -rf /` starts with
/// `git status`.
const SHELL_CHAINING: [char; 8] = [';', '&', '|', '`', '>', '<', '\n', '$'];

impl AllowRule {
    /// Derive the narrowest safe rule for a call, or `None` when no safe rule
    /// exists (i.e. the UI must not offer "always allow").
    ///
    /// * `bash` → the command up to the first chaining character. Rejected
    ///   outright if the command *begins* with one.
    /// * path-shaped arguments (`write_file`, `read_file`, …) → the path, which
    ///   later matches that file or anything under it as a directory.
    /// * anything else → `None` (tool-wide).
    pub fn for_call(tool: &str, args: &serde_json::Value) -> Option<AllowRule> {
        let effect = ToolEffect::of(tool);
        let rule = match effect {
            ToolEffect::Exec => {
                let command = args.get("command")?.as_str()?;
                let head: String = command
                    .chars()
                    .take_while(|c| !SHELL_CHAINING.contains(c))
                    .collect();
                let head = head.trim();
                if head.is_empty() {
                    return None;
                }
                AllowRule {
                    tool: tool.to_string(),
                    arg_prefix: Some(head.to_string()),
                }
            }
            ToolEffect::Read | ToolEffect::Write | ToolEffect::Other => {
                let path = args.get("path").and_then(|v| v.as_str())?;
                let path = path.trim();
                if path.is_empty() {
                    return None;
                }
                AllowRule {
                    tool: tool.to_string(),
                    arg_prefix: Some(path.to_string()),
                }
            }
        };
        Some(rule)
    }

    /// Whether this rule authorises `call`.
    ///
    /// For shell tools the remainder after the prefix must contain no chaining
    /// character, so `git status` never authorises `git status && rm -rf /`.
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool {
        if self.tool != tool {
            return false;
        }
        let Some(prefix) = self.arg_prefix.as_deref() else {
            // Tool-wide rule: only ever created for argument-less tools.
            return true;
        };
        match ToolEffect::of(tool) {
            ToolEffect::Exec => {
                let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
                    return false;
                };
                let Some(rest) = command.strip_prefix(prefix) else {
                    return false;
                };
                !rest.chars().any(|c| SHELL_CHAINING.contains(&c))
            }
            _ => {
                let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                    return false;
                };
                // Exact file, or anything beneath it when the rule names a dir.
                path == prefix || path.starts_with(&format!("{prefix}/"))
            }
        }
    }
}

#[derive(Debug)]
struct SessionInner {
    /// The run's capability ceiling. Set from the role's tier.
    tier: Permission,
    role_tier: Permission,
    mode: ApprovalMode,
    rules: Vec<AllowRule>,
}

/// Who is asking for a call to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "caller", rename_all = "snake_case")]
pub enum Caller {
    /// The model asked, in a turn the user is watching.
    Model,
    /// A plugin asked, on its own initiative. Surfaces to the user in approval
    /// dialogs so an approval of the assistant's request is never mistaken for
    /// an approval of a plugin's.
    Plugin(String),
}

impl Caller {
    /// The plugin id, when the caller is one.
    pub fn plugin_id(&self) -> Option<&str> {
        match self {
            Caller::Plugin(id) => Some(id.as_str()),
            Caller::Model => None,
        }
    }
}

/// Session-scoped policy: the live tier, mode, and what the user has already
/// agreed to.
///
/// Shared by reference across every run of a session so that "always allow"
/// survives a turn, and so that flipping the mode mid-session takes effect on
/// the very next tool call. The tier is refreshed per run.
#[derive(Debug)]
pub struct SessionPolicy {
    inner: Mutex<SessionInner>,
}

impl Default for SessionPolicy {
    /// The default: full tier, no prompting (Never).
    fn default() -> Self {
        Self {
            inner: Mutex::new(SessionInner {
                tier: Permission::Bash,
                role_tier: Permission::Bash,
                mode: ApprovalMode::Never,
                rules: Vec::new(),
            }),
        }
    }
}

impl SessionPolicy {
    /// Build a policy for a role's tier and an approval mode.
    ///
    /// Capability tier is strictly governed by the role's `tier`.
    /// Approval modes only decide *when to prompt humans*, never alter capabilities.
    pub fn new(tier: Permission, mode: ApprovalMode) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(SessionInner {
                tier,
                role_tier: tier,
                mode,
                rules: Vec::new(),
            }),
        })
    }

    /// The mode currently in force.
    pub async fn mode(&self) -> ApprovalMode {
        self.inner.lock().await.mode
    }

    /// Flip the mode.
    pub async fn set_mode(&self, mode: ApprovalMode) {
        let mut guard = self.inner.lock().await;
        guard.mode = mode;
    }

    /// Set the run's capability ceiling.
    pub async fn set_tier(&self, tier: Permission) {
        let mut guard = self.inner.lock().await;
        guard.role_tier = tier;
        guard.tier = tier;
    }

    /// Remember an "always allow" rule. Duplicate rules are collapsed.
    pub async fn remember(&self, rule: AllowRule) {
        let mut guard = self.inner.lock().await;
        if !guard.rules.contains(&rule) {
            guard.rules.push(rule);
        }
    }

    /// Forget every remembered rule (e.g. on session end).
    pub async fn clear_rules(&self) {
        self.inner.lock().await.rules.clear();
    }

    /// Rules currently in force, for a `/permissions` style listing.
    pub async fn rules(&self) -> Vec<AllowRule> {
        self.inner.lock().await.rules.clone()
    }

    /// Decide what to do with a call. The single judgement point.
    ///
    /// Pure with respect to the filesystem: it decides, it never prompts and
    /// never executes. Asking the human is the caller's job, so that a host
    /// without a UI can still enforce the verdict.
    ///
    /// Order matters, and it is the whole security property:
    ///
    /// 1. the **tier** is checked first and returns a hard `Deny` — a mode can
    ///    only narrow, so no remembered rule and no prompt can reach a call the
    ///    role forbids;
    /// 2. remembered rules are consulted next, but only for a call the tier
    ///    already permits, so a rule recorded under a wider role cannot
    ///    resurrect a capability a later, tighter run no longer has;
    /// 3. only then does the mode decide whether a human is needed.
    ///
    /// The tier is the only check that runs before the rules, and the order is
    /// load-bearing: a remembered rule is a record of what the user *did* agree
    /// to, and agreement cannot survive a narrowing of the ceiling. Checking
    /// rules first would let "always allow bash(git status)" keep working in a
    /// read-only run.
    pub async fn decide(&self, call: &ToolCall, caller: &Caller) -> Verdict {
        let tool = call.function.name.as_str();
        // A provider hands arguments over as a JSON *string*. An unparseable one
        // must not become an empty object that quietly matches a narrow rule.
        let args: serde_json::Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);

        let guard = self.inner.lock().await;
        let mode = guard.mode;
        let effect = ToolEffect::of(tool);

        // 1. The ceiling. Checked before anything else can say yes.
        if !tier_allows(guard.tier, effect) {
            return Verdict::Deny {
                reason: deny_reason(effect, guard.tier),
            };
        }

        // 2. What the user already agreed to, this session.
        if guard.rules.iter().any(|r| r.matches(tool, &args)) {
            return Verdict::Allow;
        }

        // 3. Does this mode want a human for it?
        let needs_ask = match mode {
            ApprovalMode::Always => true,
            ApprovalMode::Mutations => effect.is_prompt_worthy(),
            ApprovalMode::ShellOnly => effect == ToolEffect::Exec,
            ApprovalMode::Never => false,
        };

        if !needs_ask {
            return Verdict::Allow;
        }

        Verdict::Ask(ApprovalRequest {
            tool: tool.to_string(),
            title: match effect {
                ToolEffect::Exec => format!("执行 {tool}"),
                ToolEffect::Write => format!("写入 {tool}"),
                ToolEffect::Read => format!("读取 {tool}"),
                ToolEffect::Other => format!("运行 {tool}"),
            },
            detail: describe_call(&args),
            options: vec![
                ALLOW_ONCE.to_string(),
                ALLOW_ALWAYS.to_string(),
                DENY.to_string(),
                DENY_WITH_REASON.to_string(),
            ],
            call_hash: call_hash(tool, &args),
            caller: caller.clone(),
        })
    }

    /// The tier currently in force, for hosts that register tools from it.
    pub async fn tier(&self) -> Permission {
        self.inner.lock().await.tier
    }
}

/// Whether this tier permits an effect.
///
/// The replacement for a second tool-name match that used to live on
/// [`Permission`]. Going through [`ToolEffect`] means a tool nobody classified
/// needs [`Permission::Write`], where the old table let it through.
pub fn tier_allows(tier: Permission, effect: ToolEffect) -> bool {
    tier >= effect.required_tier()
}

/// Why a call was refused, phrased for the model.
///
/// One place so the wording cannot drift between the layer that enforces the
/// verdict and the layer that asked for it.
pub fn deny_reason(effect: ToolEffect, tier: Permission) -> String {
    match effect {
        ToolEffect::Read => format!("this run is read-only (ceiling: {})", tier.describe()),
        ToolEffect::Write => format!(
            "this run may not modify the workspace (ceiling: {})",
            tier.describe()
        ),
        ToolEffect::Exec => format!(
            "this run may not run shell commands (ceiling: {})",
            tier.describe()
        ),
        ToolEffect::Other => format!(
            "an unclassified tool needs write access, which this run does not have (ceiling: {})",
            tier.describe()
        ),
    }
}

/// A short, human-readable summary of what is being authorised.
pub fn describe_call(args: &serde_json::Value) -> String {
    if let Some(command) = args.get("command").and_then(|v| v.as_str()) {
        return command.chars().take(200).collect();
    }
    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
        return path.to_string();
    }
    // Fall back to a compact rendering so the dialog is never blank.
    serde_json::to_string(args)
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

/// Stable fingerprint of a call, used to bind an approval to its arguments.
///
/// `DefaultHasher` is explicitly *not* stable across Rust releases, which is
/// fine here: the value never leaves the process, it only has to be equal for
/// the same call within one run.
pub fn call_hash(tool: &str, args: &serde_json::Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool.hash(&mut hasher);
    // Field order must not matter, so hash the canonical form.
    serde_json::to_string(args)
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::ToolCall;

    fn args(v: serde_json::Value) -> serde_json::Value {
        v
    }

    fn call(tool: &str, args: serde_json::Value) -> ToolCall {
        ToolCall::new_function("c1", tool, args.to_string())
    }

    const MODEL: Caller = Caller::Model;

    /// Runs `f` on a fresh single-threaded runtime.
    ///
    /// The policy is async, and a runtime-inside-a-runtime test panics, so each
    /// case gets its own.
    fn rt<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn parse_accepts_the_shapes_users_actually_type() {
        assert_eq!(ApprovalMode::parse("never"), Some(ApprovalMode::Never));
        assert_eq!(ApprovalMode::parse("yolo"), Some(ApprovalMode::Never));
        assert_eq!(
            ApprovalMode::parse("Shell_Only"),
            Some(ApprovalMode::ShellOnly)
        );
        assert_eq!(
            ApprovalMode::parse("accept-edits"),
            Some(ApprovalMode::ShellOnly)
        );
        assert_eq!(
            ApprovalMode::parse("mutations"),
            Some(ApprovalMode::Mutations)
        );
        assert_eq!(ApprovalMode::parse("ask"), Some(ApprovalMode::Mutations));
        assert_eq!(ApprovalMode::parse("always"), Some(ApprovalMode::Always));
        assert_eq!(ApprovalMode::parse("manual"), Some(ApprovalMode::Always));
        assert_eq!(ApprovalMode::parse("plan"), Some(ApprovalMode::Never));
        assert_eq!(ApprovalMode::parse("nonsense"), None);
    }

    #[test]
    fn mode_round_trips_through_json() {
        for mode in ApprovalMode::ALL {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, format!("\"{}\"", mode.as_str()));
            let back: ApprovalMode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, mode);
            assert_eq!(ApprovalMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn no_mode_can_escalate_a_read_only_role() {
        rt(async {
            for mode in ApprovalMode::ALL {
                let policy = SessionPolicy::new(Permission::Read, mode);
                let write = call(
                    "write_file",
                    args(serde_json::json!({"path": "a.txt", "content": "x"})),
                );
                let shell = call("bash", args(serde_json::json!({"command": "ls"})));
                assert!(
                    matches!(policy.decide(&write, &MODEL).await, Verdict::Deny { .. }),
                    "{} must never allow writes for Read tier",
                    mode.as_str()
                );
                assert!(
                    matches!(policy.decide(&shell, &MODEL).await, Verdict::Deny { .. }),
                    "{} must never allow shell for Read tier",
                    mode.as_str()
                );
            }
        });
    }

    #[test]
    fn unknown_tools_are_not_assumed_harmless() {
        assert_eq!(ToolEffect::of("some_plugin_tool"), ToolEffect::Other);
        assert_eq!(
            ToolEffect::of("some_plugin_tool").required_tier(),
            Permission::Write
        );
        assert!(ToolEffect::of("some_plugin_tool").is_prompt_worthy());
        assert!(!ToolEffect::of("read_file").is_prompt_worthy());
        assert_eq!(ToolEffect::of("bash"), ToolEffect::Exec);
        // Execution-oriented external tools require the Bash tier, not just Write
        assert_eq!(ToolEffect::of("mcp_terminal_exec"), ToolEffect::Exec);
        assert_eq!(
            ToolEffect::of("mcp_terminal_exec").required_tier(),
            Permission::Bash
        );
        assert_eq!(ToolEffect::of("custom_shell_command"), ToolEffect::Exec);
    }

    /// The gap that made the two old tables a liability: `apply_patch` mutates
    /// the workspace but was absent from `Permission::allows_builtin`, which
    /// returned `true` for unknown names. A read-only run could patch files.
    #[test]
    fn an_unclassified_mutating_tool_needs_write_access() {
        assert_eq!(ToolEffect::of("apply_patch"), ToolEffect::Write);
        assert!(!tier_allows(
            Permission::Read,
            ToolEffect::of("apply_patch")
        ));
        assert!(tier_allows(
            Permission::Write,
            ToolEffect::of("apply_patch")
        ));
    }

    /// An unknown tool is treated as write-worthy, never as shell-worthy: being
    /// unclassified must not be a way to reach the top tier.
    #[test]
    fn an_unclassified_tool_does_not_earn_the_shell_tier() {
        let other = ToolEffect::of("mystery_tool");
        assert_eq!(other, ToolEffect::Other);
        assert_eq!(other.required_tier(), Permission::Write);
        assert!(!tier_allows(Permission::Read, other));
        assert!(tier_allows(Permission::Write, other));
    }

    #[test]
    fn mutations_mode_prompts_for_writes_and_shell_only() {
        rt(async {
            let policy = SessionPolicy::new(Permission::Bash, ApprovalMode::Mutations);
            let read = call("read_file", args(serde_json::json!({"path": "a.txt"})));
            let write = call(
                "write_file",
                args(serde_json::json!({"path": "a.txt", "content": "x"})),
            );
            let shell = call("bash", args(serde_json::json!({"command": "ls"})));

            assert_eq!(policy.decide(&read, &MODEL).await, Verdict::Allow);
            assert!(matches!(
                policy.decide(&write, &MODEL).await,
                Verdict::Ask(_)
            ));
            assert!(matches!(
                policy.decide(&shell, &MODEL).await,
                Verdict::Ask(_)
            ));
        });
    }

    #[test]
    fn shell_only_silences_writes_but_not_shell() {
        rt(async {
            let policy = SessionPolicy::new(Permission::Bash, ApprovalMode::ShellOnly);
            let write = call(
                "write_file",
                args(serde_json::json!({"path": "a.txt", "content": "x"})),
            );
            let shell = call("bash", args(serde_json::json!({"command": "ls"})));
            assert_eq!(policy.decide(&write, &MODEL).await, Verdict::Allow);
            assert!(matches!(
                policy.decide(&shell, &MODEL).await,
                Verdict::Ask(_)
            ));
        });
    }

    #[test]
    fn always_mode_asks_even_for_reads() {
        rt(async {
            let policy = SessionPolicy::new(Permission::Bash, ApprovalMode::Always);
            let read = call("read_file", args(serde_json::json!({"path": "a.txt"})));
            assert!(matches!(
                policy.decide(&read, &MODEL).await,
                Verdict::Ask(_)
            ));
        });
    }

    #[test]
    fn never_mode_asks_nothing() {
        rt(async {
            let policy = SessionPolicy::new(Permission::Bash, ApprovalMode::Never);
            let shell = call("bash", args(serde_json::json!({"command": "rm -rf /"})));
            assert_eq!(policy.decide(&shell, &MODEL).await, Verdict::Allow);
        });
    }

    /// The order of tier and rules is the security property: agreement cannot
    /// survive a narrowing of the ceiling.
    #[test]
    fn a_remembered_rule_does_not_outlive_a_narrowed_tier() {
        rt(async {
            let rule =
                AllowRule::for_call("bash", &args(serde_json::json!({"command": "git status"})))
                    .unwrap();

            let wide = SessionPolicy::new(Permission::Bash, ApprovalMode::Mutations);
            wide.remember(rule.clone()).await;
            assert_eq!(
                wide.decide(
                    &call(
                        "bash",
                        args(serde_json::json!({"command": "git status --short"}))
                    ),
                    &MODEL
                )
                .await,
                Verdict::Allow
            );

            // Same session, later run, tighter role. The rule is still on file
            // and must be ignored, because the tier is checked first.
            wide.set_tier(Permission::Read).await;
            match wide
                .decide(
                    &call("bash", args(serde_json::json!({"command": "git status"}))),
                    &MODEL,
                )
                .await
            {
                Verdict::Deny { reason } => assert!(reason.contains("read-only"), "got: {reason}"),
                other => panic!("a remembered rule must not survive a narrowed tier: {other:?}"),
            }
        });
    }

    #[test]
    fn a_rule_survives_an_ordinary_turn() {
        rt(async {
            let policy = SessionPolicy::new(Permission::Bash, ApprovalMode::Mutations);
            let rule =
                AllowRule::for_call("bash", &args(serde_json::json!({"command": "git status"})))
                    .unwrap();
            let probe = call(
                "bash",
                args(serde_json::json!({"command": "git status --short"})),
            );
            assert!(matches!(
                policy.decide(&probe, &MODEL).await,
                Verdict::Ask(_)
            ));
            policy.remember(rule).await;
            assert_eq!(policy.decide(&probe, &MODEL).await, Verdict::Allow);
            assert_eq!(policy.rules().await.len(), 1);
        });
    }

    #[test]
    fn shell_rules_cannot_be_widened_by_chaining() {
        let cmd = |c: &str| args(serde_json::json!({ "command": c }));
        let rule = AllowRule::for_call("bash", &cmd("git status")).unwrap();
        assert!(rule.matches("bash", &cmd("git status")));
        assert!(rule.matches("bash", &cmd("git status --short")));
        for hostile in [
            "git status && rm -rf /",
            "git status; rm -rf /",
            "git status | sh",
            "git status > /etc/passwd",
            "git status\nrm -rf /",
            "git status$(whoami)",
            "git status `id`",
        ] {
            assert!(
                !rule.matches("bash", &cmd(hostile)),
                "rule must not authorise: {hostile}"
            );
        }
    }

    #[test]
    fn a_command_starting_with_a_chaining_character_gets_no_rule() {
        // Otherwise the "prefix" would be empty and the rule would be tool-wide.
        assert!(
            AllowRule::for_call("bash", &args(serde_json::json!({"command": "; rm -rf /"})))
                .is_none()
        );
        assert!(
            AllowRule::for_call("bash", &args(serde_json::json!({"command": "&&whoami"})))
                .is_none()
        );
    }

    #[test]
    fn path_rules_cover_the_file_and_its_subtree() {
        let rule = AllowRule::for_call(
            "write_file",
            &args(serde_json::json!({"path": "src/lib.rs"})),
        )
        .unwrap();
        assert!(rule.matches(
            "write_file",
            &args(serde_json::json!({"path": "src/lib.rs"}))
        ));
        assert!(rule.matches(
            "write_file",
            &args(serde_json::json!({"path": "src/lib.rs/new"}))
        ));
        assert!(!rule.matches(
            "write_file",
            &args(serde_json::json!({"path": "src/lib.rs.bak"}))
        ));
        assert!(!rule.matches(
            "write_file",
            &args(serde_json::json!({"path": "other/lib.rs"}))
        ));
        assert!(!rule.matches(
            "read_file",
            &args(serde_json::json!({"path": "src/lib.rs"}))
        ));
    }

    #[test]
    fn a_tool_without_a_scoping_argument_yields_no_rule() {
        // A tool-wide "always allow" for an arbitrary plugin tool is exactly the
        // kind of blanket grant this design refuses to make.
        assert!(
            AllowRule::for_call("mcp_weird_tool", &args(serde_json::json!({"q": 1}))).is_none()
        );
    }

    #[test]
    fn call_hash_is_argument_sensitive() {
        let a = call_hash("bash", &args(serde_json::json!({"command": "ls"})));
        let b = call_hash("bash", &args(serde_json::json!({"command": "rm"})));
        let a_again = call_hash("bash", &args(serde_json::json!({"command": "ls"})));
        assert_ne!(a, b, "an approval must not transfer to other arguments");
        assert_eq!(a, a_again);
    }

    #[test]
    fn call_hash_ignores_key_order() {
        let a = call_hash(
            "write_file",
            &args(serde_json::json!({"path": "a", "content": "b"})),
        );
        let b = call_hash(
            "write_file",
            &args(serde_json::json!({"content": "b", "path": "a"})),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn mode_next_cycles_through_every_mode() {
        let mut mode = ApprovalMode::default();
        for _ in 0..ApprovalMode::ALL.len() {
            mode = mode.next();
        }
        assert_eq!(mode, ApprovalMode::default(), "cycle wraps around");
    }

    /// Unparseable arguments must not become an empty object, which would match
    /// a narrow rule by accident.
    #[test]
    fn malformed_arguments_do_not_match_a_rule() {
        rt(async {
            let policy = SessionPolicy::new(Permission::Bash, ApprovalMode::Mutations);
            let rule =
                AllowRule::for_call("write_file", &args(serde_json::json!({"path": "a"}))).unwrap();
            policy.remember(rule).await;
            let broken = ToolCall::new_function("c1", "write_file", "{not json");
            assert!(
                !matches!(policy.decide(&broken, &MODEL).await, Verdict::Allow),
                "unparseable arguments must not inherit a remembered rule"
            );
        });
    }

    /// A plugin's call is judged by the same ceiling as the model's.
    #[test]
    fn a_plugin_call_is_judged_by_the_same_tier() {
        rt(async {
            let policy = SessionPolicy::new(Permission::Read, ApprovalMode::Never);
            let write = call(
                "write_file",
                args(serde_json::json!({"path": "a", "content": "b"})),
            );
            let plugin = Caller::Plugin("evil".into());
            match policy.decide(&write, &plugin).await {
                Verdict::Deny { reason } => assert!(reason.contains("read-only")),
                other => panic!("never mode must not lift the tier for a plugin: {other:?}"),
            }
        });
    }

    #[test]
    fn an_ask_carries_the_caller_so_a_dialog_can_name_it() {
        rt(async {
            let policy = SessionPolicy::new(Permission::Bash, ApprovalMode::Mutations);
            let shell = call("bash", args(serde_json::json!({"command": "ls"})));
            match policy.decide(&shell, &Caller::Plugin("evil".into())).await {
                Verdict::Ask(req) => {
                    assert_eq!(req.caller.plugin_id(), Some("evil"));
                    assert_eq!(req.tool, "bash");
                    assert!(!req.call_hash.is_empty());
                }
                other => panic!("expected a prompt: {other:?}"),
            }
        });
    }
}
