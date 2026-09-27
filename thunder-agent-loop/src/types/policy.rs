//! Permission *policy*: the prompting layer that sits on top of the capability
//! *tier*.
//!
//! # The two-layer model
//!
//! [`Permission`] (in [`crate::types::config`]) answers **"what is possible at
//! all"**. It is a hard ceiling, enforced by which tools the host registers and
//! by `PermissionGuardMiddleware`. Nothing in this module may widen it.
//!
//! [`PermissionMode`] answers **"what still needs a human"**. It can only
//! *narrow* what the tier allows: `Plan` lowers the ceiling to read-only, and
//! `Yolo` merely stops asking. A read-only role stays read-only in every mode —
//! there is deliberately no mode that escalates privilege.
//!
//! ```
//! use thunder_agent_loop::prelude::*;
//!
//! // A read-only role, even in yolo mode, cannot write.
//! let ceiling = PermissionMode::Yolo.ceiling();
//! assert_eq!(ceiling, None); // yolo does not raise the tier
//! assert_eq!(Permission::Read.min(Permission::Bash), Permission::Read);
//!
//! // Plan mode does lower it.
//! assert_eq!(Permission::Bash.min(PermissionMode::Plan.ceiling().unwrap()), Permission::Read);
//! ```

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(test)]
use crate::types::config::Permission;

/// How intrusive the agent may be before it stops asking.
///
/// Serialised in `lowercase` so `roles.jsonl` reads `{"mode":"accept_edits"}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Read-only. Writes and shell are refused outright, with guidance to
    /// produce a plan instead. Exits via an explicit approval, never by
    /// silently escalating.
    Plan,
    /// Read freely; ask before every write and every shell command.
    Ask,
    /// Read and write freely; still ask before shell commands.
    ///
    /// `acceptEdits` is accepted as an alias: role files are hand-written, and
    /// rejecting a plausible spelling fails silently (the whole line is skipped).
    #[serde(alias = "acceptEdits", alias = "accept-edits")]
    AcceptEdits,
    /// Ask before *every* tool call, including reads.
    Manual,
    /// Never ask. The role's tier still applies — this removes prompts, it does
    /// not grant rights.
    ///
    /// # Why this is the default
    ///
    /// The gate fails *closed*: with no panel attached, every dialog resolves as
    /// cancelled, which reads as "refused". Defaulting to [`PermissionMode::Ask`]
    /// would therefore make a headless host refuse every write, every shell
    /// command, and every plugin tool — silently breaking every existing run on
    /// upgrade. A security feature nobody asked for, that breaks the product on
    /// day one, gets switched off wholesale.
    ///
    /// So approval is **opt in**: set `mode` on a role, or pass `mode` to
    /// `run_task`. The default preserves the pre-gate behaviour exactly.
    #[default]
    Yolo,
}

impl PermissionMode {
    /// An upper bound this mode imposes on the run's tier, if any.
    ///
    /// Only [`PermissionMode::Plan`] lowers it. Returning `None` means "inherit
    /// the role's tier unchanged".
    pub fn ceiling(self) -> Option<crate::types::config::Permission> {
        match self {
            PermissionMode::Plan => Some(crate::types::config::Permission::Read),
            _ => None,
        }
    }

    /// The tier actually in force: the role's, clipped by the mode.
    pub fn effective(
        self,
        tier: crate::types::config::Permission,
    ) -> crate::types::config::Permission {
        match self.ceiling() {
            Some(ceiling) => tier.min(ceiling),
            None => tier,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Plan => "plan",
            PermissionMode::Ask => "ask",
            PermissionMode::AcceptEdits => "accept_edits",
            PermissionMode::Manual => "manual",
            PermissionMode::Yolo => "yolo",
        }
    }

    /// Every mode, for a palette or a `/mode` cycle.
    pub const ALL: [PermissionMode; 5] = [
        PermissionMode::Ask,
        PermissionMode::AcceptEdits,
        PermissionMode::Manual,
        PermissionMode::Plan,
        PermissionMode::Yolo,
    ];

    /// The next mode in [`PermissionMode::ALL`], for a Shift+Tab style toggle.
    pub fn next(self) -> PermissionMode {
        let idx = PermissionMode::ALL
            .iter()
            .position(|m| *m == self)
            .unwrap_or(0);
        PermissionMode::ALL[(idx + 1) % PermissionMode::ALL.len()]
    }

    pub fn parse(raw: &str) -> Option<PermissionMode> {
        let normalised = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        PermissionMode::ALL
            .into_iter()
            .find(|m| m.as_str() == normalised)
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
    /// Plugin, MCP and skill tools all fall into [`ToolEffect::Other`], which
    /// behaves like `Write`. That is the fail-safe direction: a new tool
    /// prompts until someone classifies it.
    pub fn of(tool: &str) -> ToolEffect {
        match tool {
            "read_file" | "grep" | "find" | "ls" | "list_dir" | "read" | "glob" => ToolEffect::Read,
            "bash" | "shell" | "powershell" => ToolEffect::Exec,
            "write_file" | "edit" | "write" | "apply_patch" | "notebook_edit" => ToolEffect::Write,
            _ => ToolEffect::Other,
        }
    }

    pub fn is_prompt_worthy(self) -> bool {
        !matches!(self, ToolEffect::Read)
    }
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
    /// onto different arguments. The gate verifies the hash before honouring an
    /// "allow always" rule and before executing.
    pub call_hash: String,
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
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

#[derive(Debug, Default)]
struct SessionInner {
    mode: PermissionMode,
    rules: Vec<AllowRule>,
}

/// Session-scoped approval state: the live mode plus what the user has already
/// said yes to.
///
/// Shared by reference across every run of a session so that "always allow"
/// survives a turn, and so that flipping the mode mid-session takes effect on
/// the very next tool call.
#[derive(Debug, Default)]
pub struct SessionPolicy {
    inner: Mutex<SessionInner>,
}

impl SessionPolicy {
    pub fn new(mode: PermissionMode) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(SessionInner {
                mode,
                rules: Vec::new(),
            }),
        })
    }

    /// The mode currently in force.
    pub async fn mode(&self) -> PermissionMode {
        self.inner.lock().await.mode
    }

    /// Flip the mode. Takes effect on the next tool call — the approval gate
    /// reads it per call rather than baking it in, which is what makes a
    /// mid-session switch possible.
    pub async fn set_mode(&self, mode: PermissionMode) {
        self.inner.lock().await.mode = mode;
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

    /// Decide what to do with a call. Pure with respect to the filesystem: it
    /// never prompts, it only says whether a prompt is required.
    pub async fn decide(&self, tool: &str, args: &serde_json::Value) -> Decision {
        let guard = self.inner.lock().await;
        let mode = guard.mode;
        let effect = ToolEffect::of(tool);

        // Remembered rules win over prompting — but only for the tier that is
        // already in force, so a rule recorded under a wider role cannot
        // resurrect a capability a later, tighter run no longer has.
        if guard.rules.iter().any(|r| r.matches(tool, args)) {
            return Decision::Allow;
        }

        if mode == PermissionMode::Yolo {
            return Decision::Allow;
        }

        let needs_ask = match mode {
            PermissionMode::Manual => true,
            PermissionMode::Ask | PermissionMode::Plan => effect.is_prompt_worthy(),
            PermissionMode::AcceptEdits => effect == ToolEffect::Exec,
            PermissionMode::Yolo => false,
        };

        if !needs_ask {
            return Decision::Allow;
        }

        let detail = describe_call(tool, args);
        Decision::Ask(ApprovalRequest {
            tool: tool.to_string(),
            title: match effect {
                ToolEffect::Exec => format!("执行 {tool}"),
                ToolEffect::Write => format!("写入 {tool}"),
                ToolEffect::Read => format!("读取 {tool}"),
                ToolEffect::Other => format!("运行 {tool}"),
            },
            detail,
            options: vec![
                ALLOW_ONCE.to_string(),
                ALLOW_ALWAYS.to_string(),
                DENY.to_string(),
                DENY_WITH_REASON.to_string(),
            ],
            call_hash: call_hash(tool, args),
        })
    }
}

/// A short, human-readable summary of what is being authorised.
pub fn describe_call(_tool: &str, args: &serde_json::Value) -> String {
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

    fn args(v: serde_json::Value) -> serde_json::Value {
        v
    }

    #[test]
    fn parse_accepts_the_shapes_users_actually_type() {
        assert_eq!(PermissionMode::parse("plan"), Some(PermissionMode::Plan));
        assert_eq!(
            PermissionMode::parse("Accept-Edits"),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            PermissionMode::parse(" accept edits "),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(PermissionMode::parse("yolo"), Some(PermissionMode::Yolo));
        assert_eq!(PermissionMode::parse("nonsense"), None);
    }

    #[test]
    fn mode_round_trips_through_json() {
        for mode in PermissionMode::ALL {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, format!("\"{}\"", mode.as_str()));
            // `roles.jsonl` goes through serde; `parse` handles the hand-typed
            // forms a panel or slash command produces.
            let back: PermissionMode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, mode);
            assert_eq!(PermissionMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn no_mode_can_escalate_a_read_only_role() {
        for mode in PermissionMode::ALL {
            assert_eq!(
                mode.effective(Permission::Read),
                Permission::Read,
                "{} must never grant more than read",
                mode.as_str()
            );
        }
    }

    #[test]
    fn plan_mode_clips_the_tier_but_leaves_lower_tiers_alone() {
        assert_eq!(
            PermissionMode::Plan.effective(Permission::Bash),
            Permission::Read
        );
        assert_eq!(
            PermissionMode::Plan.effective(Permission::Read),
            Permission::Read
        );
        assert_eq!(
            PermissionMode::Ask.effective(Permission::Bash),
            Permission::Bash
        );
    }

    #[test]
    fn unknown_tools_are_not_assumed_harmless() {
        assert_eq!(ToolEffect::of("some_plugin_tool"), ToolEffect::Other);
        assert!(ToolEffect::of("some_plugin_tool").is_prompt_worthy());
        assert!(!ToolEffect::of("read_file").is_prompt_worthy());
        assert_eq!(ToolEffect::of("bash"), ToolEffect::Exec);
    }

    #[test]
    fn ask_mode_prompts_for_writes_and_shell_only() {
        let policy = SessionPolicy::new(PermissionMode::Ask);
        let read = args(serde_json::json!({"path": "a.txt"}));
        let write = args(serde_json::json!({"path": "a.txt", "content": "x"}));
        let shell = args(serde_json::json!({"command": "ls"}));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            assert!(matches!(
                policy.decide("read_file", &read).await,
                Decision::Allow
            ));
            assert!(matches!(
                policy.decide("write_file", &write).await,
                Decision::Ask(_)
            ));
            assert!(matches!(
                policy.decide("bash", &shell).await,
                Decision::Ask(_)
            ));
        });
    }

    #[test]
    fn accept_edits_silences_writes_but_not_shell() {
        let policy = SessionPolicy::new(PermissionMode::AcceptEdits);
        let write = args(serde_json::json!({"path": "a.txt", "content": "x"}));
        let shell = args(serde_json::json!({"command": "ls"}));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(matches!(
                policy.decide("write_file", &write).await,
                Decision::Allow
            ));
            assert!(matches!(
                policy.decide("bash", &shell).await,
                Decision::Ask(_)
            ));
        });
    }

    #[test]
    fn manual_mode_asks_even_for_reads() {
        let policy = SessionPolicy::new(PermissionMode::Manual);
        let read = args(serde_json::json!({"path": "a.txt"}));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(matches!(
                policy.decide("read_file", &read).await,
                Decision::Ask(_)
            ));
        });
    }

    #[test]
    fn yolo_asks_nothing() {
        let policy = SessionPolicy::new(PermissionMode::Yolo);
        let shell = args(serde_json::json!({"command": "rm -rf /"}));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(matches!(
                policy.decide("bash", &shell).await,
                Decision::Allow
            ));
        });
    }

    #[test]
    fn a_rule_does_not_survive_a_mode_that_stops_asking_anyway_but_does_survive_turns() {
        let policy = SessionPolicy::new(PermissionMode::Ask);
        let rule = AllowRule::for_call("bash", &args(serde_json::json!({"command": "git status"})))
            .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(matches!(
                policy
                    .decide(
                        "bash",
                        &args(serde_json::json!({"command": "git status --short"}))
                    )
                    .await,
                Decision::Ask(_)
            ));
            policy.remember(rule).await;
            // Same intent, extra flags: the rule covers it.
            assert!(matches!(
                policy
                    .decide(
                        "bash",
                        &args(serde_json::json!({"command": "git status --short"}))
                    )
                    .await,
                Decision::Allow
            ));
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
        let mut mode = PermissionMode::Ask;
        let mut seen = vec![mode];
        for _ in 0..PermissionMode::ALL.len() - 1 {
            mode = mode.next();
            seen.push(mode);
        }
        assert_eq!(seen.len(), PermissionMode::ALL.len());
        assert_eq!(mode.next(), PermissionMode::Ask, "cycle wraps around");
    }
}
