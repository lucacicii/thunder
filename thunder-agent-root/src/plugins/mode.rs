//! The mode plugin: the host's judge, as a plugin.
//!
//! The kernel asks; this plugin answers. What it carries is every product
//! opinion the kernel used to hold in its own `policy` module — the four modes,
//! the remembered "always allow" rules, the approval wording — so removing it
//! from a host's plugin list really does remove the opinions: with no gate
//! installed, everything the kernel's guard allows runs.
//!
//! The modes line up with Claude Code's, because they encode the same trade:
//!
//! | mode          | reads | writes in scope | writes out of scope | shell |
//! |---------------|-------|-----------------|---------------------|-------|
//! | `plan`        | allow | deny            | deny                | ask   |
//! | `ask`         | allow | ask             | ask                 | ask   |
//! | `acceptEdits` | allow | allow           | ask                 | ask   |
//! | `yolo`        | allow | allow           | allow               | allow |
//!
//! Two invariants keep the table honest. In `plan`, a non-read call is denied
//! *even when a rule matches* — an "always allow" recorded in a freer mode is
//! not a way to write in plan mode. And in every mode the kernel's guard has
//! already run: this gate is asked only about calls that cleared it, so no
//! matrix entry can make `rm -rf /` runnable.

use crate::plugin::{PluginManifest, ThunderPlugin};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::RwLock;
use thunder_agent_loop::types::gate::{
    ApprovalAnswer, ApprovalPrompt, GateVerdict, ToolGate, ToolGateRequest,
};
use thunder_agent_loop::types::policy::ToolEffect;
use thunder_agent_loop::types::scope::Scope;

/// What the agent may do without asking, this mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Read-only. Writes are refused with the way out named; shell is asked
    /// about, because inspecting is ordinary work in a planning phase.
    Plan,
    /// Ask before anything that changes something.
    Ask,
    /// Auto-accept edits inside the working area; ask about the rest.
    AcceptEdits,
    /// Ask nothing. The kernel's guard still applies — this mode loosens the
    /// gate, never the floor.
    ///
    /// The default, because it is what "no mode chosen" has always meant: the
    /// pre-0.4 kernel defaulted to `Never`, and every headless consumer
    /// (daemon runs without a panel, embedders without a UI) depends on a
    /// default that does not ask questions nobody can answer. Hosts that want
    /// asking install the mode they want, or set `agent.defaultMode`.
    #[default]
    Yolo,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Ask => "ask",
            Mode::AcceptEdits => "acceptEdits",
            Mode::Yolo => "yolo",
        }
    }

    /// Every mode, in Shift+Tab order.
    pub const ALL: [Mode; 4] = [Mode::Plan, Mode::Ask, Mode::AcceptEdits, Mode::Yolo];

    /// The next mode in [`Mode::ALL`], for a Shift+Tab style toggle.
    pub fn next(self) -> Mode {
        let idx = Mode::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Mode::ALL[(idx + 1) % Mode::ALL.len()]
    }

    /// Parse what users and configs actually write.
    ///
    /// The old names still resolve, so a config file written before 0.4 keeps
    /// meaning what its author meant. The one correction is deliberate:
    /// `plan` used to be parsed as `never`, which made the least cautious mode
    /// out of the most cautious word. It is the plan mode now.
    pub fn parse(raw: &str) -> Option<Mode> {
        let normalised = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match normalised.as_str() {
            "plan" => Some(Mode::Plan),
            "ask" | "default" | "manual" | "mutations" | "writes" | "always" | "all" => {
                Some(Mode::Ask)
            }
            "acceptedits" | "accept_edits" | "shell_only" | "ask_shell" => Some(Mode::AcceptEdits),
            "yolo" | "never" | "none" | "bypass" | "bypass_permissions" | "bypasspermissions" => {
                Some(Mode::Yolo)
            }
            _ => None,
        }
    }

    /// The word the system prompt uses for what this mode is.
    pub fn describe(self) -> &'static str {
        match self {
            Mode::Plan => "plan mode: gather information and present a plan; nothing is modified",
            Mode::Ask => "ask mode: changes run only after the user approves them",
            Mode::AcceptEdits => "acceptEdits mode: edits inside the working area run without asking; everything else asks",
            Mode::Yolo => "yolo mode: the user has chosen not to be asked",
        }
    }
}

/// A remembered "always allow" rule, scoped to one session.
///
/// The rule is deliberately narrow: it pins one tool and, where meaningful, the
/// *prefix* of its primary argument. A rule never grants a tool class — which is
/// also why some calls get no rule and therefore no "always allow" option at
/// all, rather than a blanket grant this design refuses to make.
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
    /// exists (i.e. the dialog must not offer "always allow").
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
            _ => {
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

    /// What the trace shows for a call that ran on this rule's authority.
    pub fn label(&self) -> String {
        match &self.arg_prefix {
            Some(prefix) => format!("{} {}", self.tool, prefix),
            None => self.tool.clone(),
        }
    }
}

/// The gate itself, and the state that outlives a call: the mode and the rules
/// the user has agreed to this session.
///
/// Shared by reference across every run of a session so an "always allow"
/// survives a turn, and so flipping the mode takes effect on the very next
/// call — the kernel asks the gate every time and caches nothing.
#[derive(Debug)]
pub struct ModePlugin {
    mode: RwLock<Mode>,
    rules: RwLock<Vec<AllowRule>>,
    manifest: PluginManifest,
}

impl ModePlugin {
    pub fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode: RwLock::new(mode),
            rules: RwLock::new(Vec::new()),
            manifest: PluginManifest::new(
                "mode",
                "Thunder Mode Gate",
                "Judges tool calls against the active mode (plan / ask / acceptEdits / yolo) and \
                 the rules the user has agreed to this session.",
                "0.1.0",
            ),
        })
    }

    /// The mode currently in force.
    pub fn mode(&self) -> Mode {
        *self
            .mode
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Flip the mode. The next call is judged by it.
    pub fn set_mode(&self, mode: Mode) {
        *self
            .mode
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = mode;
    }

    /// Rules currently in force, for a listing.
    pub fn rules(&self) -> Vec<AllowRule> {
        self.rules
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Forget every remembered rule (e.g. on session end).
    pub fn clear_rules(&self) {
        self.rules
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// Record a rule the user agreed to, outside the dialog path.
    ///
    /// `record_answer` is the normal way in; this one exists for hosts that
    /// seed a session with standing grants of their own.
    pub fn remember(&self, rule: AllowRule) {
        let mut rules = self
            .rules
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }

    /// The prompt for a call this mode wants to ask about.
    fn ask_for(&self, request: &ToolGateRequest<'_>) -> GateVerdict {
        let detail = thunder_agent_loop::types::policy::describe_call(request.arguments);
        GateVerdict::Ask(ApprovalPrompt {
            title: match request.effect {
                ToolEffect::Exec => format!("Run {}", request.tool),
                ToolEffect::Write => format!("Write {}", request.tool),
                ToolEffect::Read => format!("Read {}", request.tool),
                ToolEffect::Other => format!("Use {}", request.tool),
            },
            detail,
            // "Always allow" is offered only when this call can be narrowed to
            // a rule narrow enough to show the user what they are agreeing to.
            allow_always: AllowRule::for_call(request.tool, request.arguments).is_some(),
        })
    }
}

impl ToolGate for ModePlugin {
    fn decide(&self, request: &ToolGateRequest<'_>) -> GateVerdict {
        let mode = self.mode();

        // What the user already agreed to, this session — consulted before the
        // mode, but never in plan mode: a rule recorded in a freer mode must
        // not be a way to write while planning. (Claude Code is as blunt about
        // it: file edits are never auto-approved in plan mode, even when an
        // allow rule matches.)
        if mode != Mode::Plan {
            if let Some(rule) = self
                .rules
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .find(|r| r.matches(request.tool, request.arguments))
            {
                return GateVerdict::AllowByRule { rule: rule.label() };
            }
        }

        match mode {
            Mode::Yolo => GateVerdict::Allow,
            Mode::Ask => match request.effect {
                ToolEffect::Read => GateVerdict::Allow,
                _ => self.ask_for(request),
            },
            Mode::AcceptEdits => match (request.effect, request.scope) {
                (ToolEffect::Read, _) => GateVerdict::Allow,
                // Scope may only relax: an in-scope write is the one thing this
                // mode was named for. Out-of-scope and unknown both ask — a
                // classification the host did not make is not one to lean on.
                (ToolEffect::Write, Scope::InScope) => GateVerdict::Allow,
                _ => self.ask_for(request),
            },
            Mode::Plan => match request.effect {
                ToolEffect::Read => GateVerdict::Allow,
                // Inspecting is ordinary plan work, so shell asks rather than
                // denies — the question a user can answer is "yes, look".
                ToolEffect::Exec => self.ask_for(request),
                _ => GateVerdict::Deny {
                    reason: format!(
                        "plan mode: only read-only tools run here. '{}' changes something, so it \
                         is refused — finish investigating, then leave plan mode before making \
                         changes.",
                        request.tool
                    ),
                },
            },
        }
    }

    fn record_answer(&self, request: &ToolGateRequest<'_>, answer: &ApprovalAnswer) {
        if let ApprovalAnswer::AllowAlways = answer {
            if let Some(rule) = AllowRule::for_call(request.tool, request.arguments) {
                self.remember(rule);
            }
            // No safe rule exists ⇒ the kernel did not offer the option; an
            // answer claiming it anyway narrows to nothing, by design.
        }
    }

    fn label(&self) -> &str {
        self.mode().as_str()
    }
}

#[async_trait]
impl ThunderPlugin for ModePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    // `write_plan` / `exit_plan_mode` and the mode's prompt contribution arrive
    // with plan mode itself; the gate is deliberately useful without them.
}
