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
//! | mode          | reads | writes in scope | writes out of scope | shell                 |
//! |---------------|-------|-----------------|---------------------|-----------------------|
//! | `plan`        | allow | deny            | deny                | read: allow, else ask |
//! | `ask`         | allow | ask             | ask                 | ask                   |
//! | `acceptEdits` | allow | allow           | ask                 | ask                   |
//! | `yolo`        | allow | allow           | allow               | allow                 |
//!
//! "read: allow" in plan's shell column is the one cell that is not a word
//! about the tool but about the *command*: a call the kernel read and proved
//! changes nothing runs unattended, and everything else asks as it always did.
//! Inspecting is ordinary plan work, and a dialog asking "may I look?" is a
//! question the user already answered by choosing plan mode. The proof lives in
//! [`is_read_only`](thunder_agent_policy::read_only::is_read_only), and it
//! errs towards asking: one separator, one flag that writes, one credential in
//! the arguments, and the dialog appears.
//!
//! Two invariants keep the table honest. In `plan`, a non-read call is denied
//! *even when a rule matches* — an "always allow" recorded in a freer mode is
//! not a way to write in plan mode. And in every mode the kernel's guard has
//! already run: this gate is asked only about calls that cleared it, so no
//! matrix entry can make `rm -rf /` runnable.
//!
//! Plan mode has one door out and one thing it may write: the tools this
//! plugin registers ([`WRITE_PLAN_TOOL`], [`EXIT_PLAN_MODE_TOOL`]) are exempt
//! in every mode — by name, never by the effect a tool declares about itself.

use super::permission_rules;
use crate::assembly::{Prepared, PromptSection, RunEnv};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use thunder_agent_pack_code::builtin::fs::WriteFileTool;
use thunder_agent_policy::gate::{
    ApprovalAnswer, ApprovalPrompt, GateVerdict, ToolGate, ToolGateRequest,
};
use thunder_agent_policy::plugin::{
    CallToolResult, InterceptorPlugin, Plugin as CorePlugin, PreExecutionVerdict, ToolPlugin,
};
use thunder_agent_policy::read_only::call_is_read_only;
use thunder_agent_policy::ui::{NullHostUi, UiRequest, UiSource};
use thunder_agent_policy::{AgentTool, ToolExecutionContext};
use thunder_agent_types::policy::{Caller, ToolEffect};
use thunder_agent_types::scope::Scope;
use thunder_agent_types::tool::ToolDefinition;

/// What the agent may do without asking, this mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Read-only. Writes are refused with the way out named; shell is asked
    /// about unless the command itself was proved to only read, because
    /// inspecting is ordinary work in a planning phase.
    Plan,
    /// Ask before anything that changes something.
    ///
    /// The default: a host that never chose a mode asks rather than silently
    /// running. Hosts without a panel still fail closed (every dialog refuses),
    /// and hosts that want autonomy say so — `agent.defaultMode`, a pinned run
    /// mode, or `yolo` itself. The kernel's no-gate contract is unaffected: no
    /// gate installed still means "everything the guard allows runs", which is
    /// what headless embedders (the JSONL runtime) rely on.
    #[default]
    Ask,
    /// Auto-accept edits inside the working area; ask about the rest.
    AcceptEdits,
    /// Ask nothing. The kernel's guard still applies — this mode loosens the
    /// gate, never the floor.
    ///
    /// Explicit opt-in only: it must never be what "no mode chosen" means.
    /// `PluginContext::gate()` keeps an explicit `Yolo` fallback for hosts
    /// that installed no mode plugin at all, so this default cannot lock a
    /// gateless host behind dialogs nobody can answer.
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

/// The tool names this plugin registers. The gate exempts these *names* — see
/// the note on [`ModePlugin`]'s `decide`.
pub const WRITE_PLAN_TOOL: &str = "write_plan";
pub const EXIT_PLAN_MODE_TOOL: &str = "exit_plan_mode";

/// Custom event `kind` raised whenever the mode a session is judged by changes.
///
/// It lives on this side because the one mode change the daemon cannot see is
/// the plan approval: [`ExitPlanModeTool`] holds an `Arc<ModeState>`, not the
/// plugin, so it flips the mode directly and reports the transition through the
/// run's event sink. The daemon re-shapes that custom event into the same
/// pushed frame a client-driven `set_mode` produces, so a panel has one
/// contract to consume.
pub const MODE_CHANGED_EVENT_KIND: &str = "mode_changed";

/// The `source` word for a mode change the approval dialog made. Spelled here
/// because this is what emits it; the daemon's own `ModeChangeSource` enum
/// serialises to the same words.
pub const MODE_CHANGE_SOURCE_PLAN_APPROVED: &str = "plan_approved";

/// What the plan review offers, in the order the options appear. These are the
/// strings on the wire and in the dialog; the answers are matched against
/// them, so a translation of the panel must answer with the option it showed.
pub const APPROVE_AND_AUTO_ACCEPT: &str = "Yes, and auto-accept edits";
pub const APPROVE_BUT_ASK: &str = "Yes, but ask before each edit";
pub const APPROVE_FULLY_AUTONOMOUS: &str = "Yes, fully autonomous";
pub const KEEP_PLANNING: &str = "No, keep planning";

/// Reduce a session id to a safe filename: `[A-Za-z0-9._-]`, everything else
/// `_`.
///
/// The session id arrives from whoever opened the session (the daemon's comes
/// from the client), and the path jail is gone — this filter is what keeps a
/// hostile id from walking out of the plans directory. Replacing `/` (and
/// every other path-shaped character) with `_` leaves no separator to exploit;
/// what survives is a name *in* the directory, and the two names that would
/// not be (`.` and `..`) are replaced outright.
fn sanitize_session_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match cleaned.trim_matches('_') {
        "" | "." | ".." => "session".to_string(),
        kept => kept.to_string(),
    }
}

/// Where this session's plan lives: `<workspace>/.thunder/plans/<session>.md`.
fn plan_path(workspace: &Path, session_id: &str) -> PathBuf {
    workspace
        .join(".thunder")
        .join("plans")
        .join(format!("{}.md", sanitize_session_id(session_id)))
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
    ///
    /// Both keys are stored *normalised*
    /// ([`permission_rules`](super::permission_rules)), because a rule that only
    /// paid for the spelling the dialog happened to show would ask again for
    /// `./a.txt` after the user approved `a.txt`. `workspace` is what lets the
    /// two spellings be recognised as one file; see
    /// [`normalize_path`](super::permission_rules::normalize_path).
    pub fn for_call(
        tool: &str,
        args: &serde_json::Value,
        workspace: Option<&Path>,
    ) -> Option<AllowRule> {
        let effect = ToolEffect::of(tool);
        let rule = match effect {
            ToolEffect::Exec => {
                let command = args.get("command")?.as_str()?;
                let command = permission_rules::normalize_command(command);
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
                let path = permission_rules::normalize_path(path, workspace);
                if path.is_empty() {
                    return None;
                }
                AllowRule {
                    tool: tool.to_string(),
                    arg_prefix: Some(path),
                }
            }
        };
        Some(rule)
    }

    /// Whether this rule authorises `call`.
    ///
    /// For shell tools the remainder after the prefix must contain no chaining
    /// character, so `git status` never authorises `git status && rm -rf /`.
    ///
    /// The call is normalised exactly as the rule was
    /// ([`for_call`](Self::for_call)), which is what makes a rule survive the
    /// model writing the same target another way.
    pub fn matches(&self, tool: &str, args: &serde_json::Value, workspace: Option<&Path>) -> bool {
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
                let command = permission_rules::normalize_command(command);
                let Some(rest) = command.strip_prefix(prefix) else {
                    return false;
                };
                !rest.chars().any(|c| SHELL_CHAINING.contains(&c))
            }
            _ => {
                let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                    return false;
                };
                let path = permission_rules::normalize_path(path, workspace);
                // Exact file, or anything beneath it when the rule names a dir.
                permission_rules::under(&path, prefix)
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

/// Append `rule` unless it is already there, reporting whether it was added.
///
/// The two ways a rule gets in — the answer the user gave a dialog, and the file
/// a previous session wrote — share this, so "remembered" describes one list
/// however it was filled.
fn push_unique(rules: &mut Vec<AllowRule>, rule: AllowRule) -> bool {
    if rules.contains(&rule) {
        return false;
    }
    rules.push(rule);
    true
}

/// The state a session's mode is made of: the mode, the rules the user agreed
/// to, and where this session's plan lives.
///
/// Shared between the plugin and the tools it registers, so `exit_plan_mode`
/// flipping the mode is the same lock `decide` reads — the flip lands on the
/// very next call, with no rebuild in between.
#[derive(Debug, Default)]
struct ModeState {
    mode: RwLock<Mode>,
    rules: RwLock<Vec<AllowRule>>,
    workspace: RwLock<Option<PathBuf>>,
    session_id: RwLock<String>,
    /// Where agreed rules are written, when this plugin was given a file. `None`
    /// (the default, and what [`ModePlugin::new`] leaves) keeps them in memory
    /// only — the behaviour every test and embedder had before there was a file.
    rules_path: RwLock<Option<PathBuf>>,
}

impl ModeState {
    /// The mode currently in force.
    fn mode(&self) -> Mode {
        *self
            .mode
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Where the session's files are, when the host bound a workspace.
    ///
    /// Read by rule derivation and matching, which need it to see `a.txt` and
    /// `/ws/a.txt` as one file.
    fn workspace(&self) -> Option<PathBuf> {
        self.workspace
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Flip the mode. The next call is judged by it.
    fn set_mode(&self, mode: Mode) {
        *self
            .mode
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = mode;
    }

    /// Where this session's plan lives, if a workspace is bound.
    ///
    /// An unbound session (a host with no working area) cannot plan to a file;
    /// the message says so plainly rather than inventing a location.
    fn plan_path(&self) -> Result<PathBuf, String> {
        let workspace = self
            .workspace
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let session_id = self
            .session_id
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        match workspace {
            Some(ws) => Ok(plan_path(&ws, &session_id)),
            None => Err(
                "No workspace is bound to this session, so there is nowhere to keep the plan. \
                 Plan mode needs a working area; ask the user to run this somewhere that has one."
                    .to_string(),
            ),
        }
    }
}

/// The gate itself, and the state that outlives a call: the mode and the rules
/// the user has agreed to this session.
///
/// Shared by reference across every run of a session so an "always allow"
/// survives a turn, and so flipping the mode takes effect on the very next
/// call — the kernel asks the gate every time and caches nothing.
#[derive(Debug, Clone)]
pub struct ModePlugin {
    state: Arc<ModeState>,
}

impl ModePlugin {
    pub fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(ModeState {
                mode: RwLock::new(mode),
                ..ModeState::default()
            }),
        })
    }

    pub const ID: &'static str = "mode";

    /// The plan-mode tools: record the plan, submit it.
    pub fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        vec![
            Arc::new(WritePlanTool {
                state: Arc::clone(&self.state),
            }),
            Arc::new(ExitPlanModeTool {
                state: Arc::clone(&self.state),
            }),
        ]
    }

    /// The mode currently in force.
    pub fn mode(&self) -> Mode {
        self.state.mode()
    }

    /// Flip the mode. The next call is judged by it.
    pub fn set_mode(&self, mode: Mode) {
        self.state.set_mode(mode);
    }

    /// Flip the mode and report the transition, or `None` when this session was
    /// already in `mode`.
    ///
    /// Every caller that can change a *user's* session mode goes through here,
    /// so "a no-op raises no event" is one rule in one place. That matters
    /// because the host re-applies the run's mode at every run start
    /// (`host.rs`), which would otherwise look like a change every turn.
    ///
    /// A caller that only calls [`Self::set_mode`] is deliberately not
    /// announced; the plan approval is the one such caller, and it reports
    /// through the run's event sink instead — see [`MODE_CHANGED_EVENT_KIND`].
    pub fn set_mode_announcing(&self, mode: Mode) -> Option<Mode> {
        let previous = self.mode();
        if previous == mode {
            return None;
        }
        self.state.set_mode(mode);
        Some(previous)
    }

    /// Bind the session this plugin judges: where the plan file lives and what
    /// it is named after. Called by the host at every run start, so a reused
    /// plugin is always bound to the session it is judging.
    pub fn bind_session(&self, workspace: Option<PathBuf>, session_id: &str) {
        let state = &self.state;
        *state
            .workspace
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = workspace;
        *state
            .session_id
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = session_id.to_string();
    }

    /// A plugin that remembers across sessions.
    ///
    /// The same gate as [`ModePlugin::new`], plus the standing rules a user has
    /// already agreed to: they are read from
    /// [`rules_path`](super::permission_rules::rules_path) now, and every
    /// "always allow" from here on is written back there. Without this
    /// constructor — a test, an embedder, a headless run — rules stay in memory,
    /// which is what "always allow" meant before there was a file to keep it in.
    ///
    /// Note what does *not* change: the rules are still only consulted outside
    /// plan mode, and still only for calls the guard let through.
    pub fn with_persisted_rules(mode: Mode) -> Arc<Self> {
        let plugin = Self::new(mode);
        plugin.load_rules_from(permission_rules::rules_path());
        plugin
    }

    /// Read standing rules from `path` and keep writing there from now on.
    ///
    /// Called once per session, when the session's policy is created. The rules
    /// go into the list directly rather than through [`remember`] so that
    /// loading never writes: the file was just read, and rewriting it on start-up
    /// would be traffic with nothing to say.
    ///
    /// [`remember`]: Self::remember
    pub fn load_rules_from(&self, path: PathBuf) {
        let loaded = permission_rules::load(&path);
        {
            let mut rules = self
                .state
                .rules
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for rule in loaded {
                push_unique(&mut rules, rule);
            }
        }
        *self
            .state
            .rules_path
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
    }

    /// Rules currently in force, for a listing.
    pub fn rules(&self) -> Vec<AllowRule> {
        self.state
            .rules
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Forget every remembered rule (e.g. on session end), on disk as well.
    pub fn clear_rules(&self) {
        let snapshot = {
            let mut rules = self
                .state
                .rules
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            rules.clear();
            rules.clone()
        };
        self.persist_replacing(&snapshot);
    }

    /// Record a rule the user agreed to, outside the dialog path.
    ///
    /// `record_answer` is the normal way in; this one exists for hosts that
    /// seed a session with standing grants of their own.
    pub fn remember(&self, rule: AllowRule) {
        let snapshot = {
            let mut rules = self
                .state
                .rules
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !push_unique(&mut rules, rule) {
                // Already remembered: nothing changed, so there is nothing to
                // write either.
                return;
            }
            rules.clone()
        };
        self.persist(&snapshot);
    }

    /// Add the rules to the file, keeping what another session already wrote.
    ///
    /// A failure is swallowed on purpose. It cannot fail the call the user just
    /// approved — that answer is already in memory and is what the next call is
    /// judged by — and a permission file that cannot be written is a smaller
    /// problem than a run that dies remembering one.
    fn persist(&self, rules: &[AllowRule]) {
        if let Some(path) = self.rules_file() {
            let _ = permission_rules::merge(&path, rules);
        }
    }

    /// Replace the file's contents, for a clearing: here the point *is* removal,
    /// so merging would put back what is being forgotten.
    fn persist_replacing(&self, rules: &[AllowRule]) {
        if let Some(path) = self.rules_file() {
            let _ = permission_rules::save(&path, rules);
        }
    }

    /// Where to write rules, when this plugin was given a file.
    fn rules_file(&self) -> Option<PathBuf> {
        self.state
            .rules_path
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The prompt for a call this mode wants to ask about.
    fn ask_for(&self, request: &ToolGateRequest<'_>) -> GateVerdict {
        let detail = thunder_agent_types::policy::describe_call(request.arguments);
        GateVerdict::Ask(ApprovalPrompt {
            title: match request.effect {
                ToolEffect::Exec => format!("Run {}", request.tool),
                ToolEffect::Write | ToolEffect::PlanArtifact => {
                    format!("Write {}", request.tool)
                }
                ToolEffect::Read => format!("Read {}", request.tool),
                ToolEffect::Other => format!("Use {}", request.tool),
            },
            detail,
            // "Always allow" is offered only when this call can be narrowed to
            // a rule narrow enough to show the user what they are agreeing to.
            allow_always: AllowRule::for_call(
                request.tool,
                request.arguments,
                self.state.workspace().as_deref(),
            )
            .is_some(),
        })
    }
}

impl ToolGate for ModePlugin {
    fn decide(&self, request: &ToolGateRequest<'_>) -> GateVerdict {
        let mode = self.mode();

        // The plan tools are exempt by *name*, never by the effect a tool
        // declares about itself: the effect is the tool's own word for what it
        // does, so an exemption keyed on `PlanArtifact` would let any tool opt
        // itself out of the matrix by claiming the effect. The names are pinned
        // because they are the ones this plugin registers — `write_plan` writes
        // only the plan file, and `exit_plan_mode` changes the mode only
        // through the approval dialog it raises itself.
        if request.tool == WRITE_PLAN_TOOL || request.tool == EXIT_PLAN_MODE_TOOL {
            return GateVerdict::Allow;
        }

        // What the user already agreed to, this session — consulted before the
        // mode, but never in plan mode: a rule recorded in a freer mode must
        // not be a way to write while planning. (Claude Code is as blunt about
        // it: file edits are never auto-approved in plan mode, even when an
        // allow rule matches.)
        if mode != Mode::Plan {
            let workspace = self.state.workspace();
            if let Some(rule) = self
                .state
                .rules
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .find(|r| r.matches(request.tool, request.arguments, workspace.as_deref()))
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
                // denies — the question a user can answer is "yes, look". A
                // command the kernel read to the end and proved changes nothing
                // never reaches that question: asking about `ls` is noise, and
                // looking around is the one thing a planning phase does
                // constantly.
                ToolEffect::Exec if call_is_read_only(request.arguments) => GateVerdict::Allow,
                ToolEffect::Exec => self.ask_for(request),
                _ => GateVerdict::Deny {
                    // The exit has to be named, or the refusal reads as "try
                    // again differently" — and with every tool registered, the
                    // model *will* see `write_file` and try it.
                    reason: format!(
                        "plan mode: only read-only tools run here. '{}' changes something, so it \
                         is refused. Write the plan with `{}`, then submit it with `{}` — the \
                         user's approval is the only way out of plan mode.",
                        request.tool, WRITE_PLAN_TOOL, EXIT_PLAN_MODE_TOOL
                    ),
                },
            },
        }
    }

    fn record_answer(&self, request: &ToolGateRequest<'_>, answer: &ApprovalAnswer) {
        if let ApprovalAnswer::AllowAlways = answer {
            if let Some(rule) = AllowRule::for_call(
                request.tool,
                request.arguments,
                self.state.workspace().as_deref(),
            ) {
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

impl CorePlugin for ModePlugin {
    fn id(&self) -> &str {
        "mode_interceptor"
    }

    fn name(&self) -> &str {
        "Mode Policy Interceptor"
    }

    fn description(&self) -> &str {
        "Enforces plan, ask, acceptEdits, and yolo permission modes before tool execution."
    }
}

#[async_trait]
impl InterceptorPlugin for ModePlugin {
    fn priority(&self) -> i32 {
        thunder_agent_policy::plugin::priority::GOVERNANCE_MODE
    }

    async fn pre_execute(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<PreExecutionVerdict, String> {
        let effect = ToolEffect::of(tool_name);
        let caller = match ctx.caller.as_deref() {
            Some(id) => Caller::Plugin(id.to_string()),
            None => Caller::Model,
        };
        let req = ToolGateRequest {
            tool: tool_name,
            caller,
            effect,
            arguments: args,
            scope: Scope::Unknown,
            tool_call_id: &ctx.tool_call_id,
        };
        match self.decide(&req) {
            GateVerdict::Allow | GateVerdict::AllowByRule { .. } => Ok(PreExecutionVerdict::Allow),
            GateVerdict::Deny { reason } => Ok(PreExecutionVerdict::Deny { reason }),
            GateVerdict::Ask(prompt) => Ok(PreExecutionVerdict::AskUser {
                prompt: format!("{}: {}", prompt.title, prompt.detail),
            }),
        }
    }
}

/// `write_plan`: the one write plan mode allows, and it is not the model's to
/// choose — the path comes from the session, not the arguments.
struct WritePlanTool {
    state: Arc<ModeState>,
}

#[async_trait]
impl AgentTool for WritePlanTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            WRITE_PLAN_TOOL,
            "Write this session's plan, replacing the previous one. The only write allowed in \
             plan mode; submit it with exit_plan_mode.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The full plan, markdown."
                    }
                },
                "required": ["content"]
            }),
        )
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::PlanArtifact
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "Missing required field 'content' (the full plan, markdown)".to_string()
            })?
            .to_string();
        let path = self.state.plan_path()?;
        // The same atomic write every other write goes through: one lock per
        // path, staged, flushed, renamed — and cancellable before the commit.
        WriteFileTool::default()
            .write_cancellable(&path.to_string_lossy(), &content, &ctx.cancellation_token)
            .await?;
        Ok(format!(
            "Plan written to {} ({} bytes). Call `{}` to submit it for approval.",
            path.display(),
            content.len(),
            EXIT_PLAN_MODE_TOOL
        ))
    }
}

impl CorePlugin for WritePlanTool {
    fn id(&self) -> &str {
        WRITE_PLAN_TOOL
    }
}

#[async_trait]
impl ToolPlugin for WritePlanTool {
    fn definition(&self) -> ToolDefinition {
        AgentTool::definition(self)
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::PlanArtifact
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String> {
        AgentTool::execute(self, args, ctx)
            .await
            .map(CallToolResult::success_text)
    }
}

/// `exit_plan_mode`: submit the plan for approval. The only way out, and the
/// user — not the model — picks the mode it lands in.
struct ExitPlanModeTool {
    state: Arc<ModeState>,
}

#[async_trait]
impl AgentTool for ExitPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            EXIT_PLAN_MODE_TOOL,
            "Submit the plan for the user's approval and leave plan mode. Takes no arguments; \
             the plan is read from where write_plan put it.",
            serde_json::json!({"type": "object", "properties": {}}),
        )
    }

    // Reading the plan back is a read. The mode flip is not this tool's to
    // make: it happens only when the user approves, in the dialog below.
    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let path = self.state.plan_path()?;
        let plan = tokio::fs::read_to_string(&path).await.map_err(|_| {
            format!(
                "No plan file at {}. Write it with `{}` first.",
                path.display(),
                WRITE_PLAN_TOOL
            )
        })?;
        if plan.trim().is_empty() {
            return Err(format!(
                "The plan file at {} is empty. Write the plan with `{}` first.",
                path.display(),
                WRITE_PLAN_TOOL
            ));
        }

        let ui = ctx
            .ui
            .as_ref()
            .map(|handle| handle.ui())
            .unwrap_or_else(|| Arc::new(NullHostUi));
        let answer = ui
            .request(
                UiSource::Host,
                UiRequest::Review {
                    title: "Approve this plan?".to_string(),
                    path: path.display().to_string(),
                    content: plan,
                    options: vec![
                        APPROVE_AND_AUTO_ACCEPT.to_string(),
                        APPROVE_BUT_ASK.to_string(),
                        APPROVE_FULLY_AUTONOMOUS.to_string(),
                        KEEP_PLANNING.to_string(),
                    ],
                    timeout_ms: None,
                },
            )
            .await;

        // A review nobody answered is not an error: failing closed here means
        // staying in plan mode, and an `is_error` result would read as "retry
        // the call" — the one move that must not follow.
        let Some(choice) = answer.text() else {
            return Ok("Decision: dismissed (no answer, source: plan_approved). \
                        No answer (dismissed or timed out). Still in plan mode."
                .to_string());
        };

        match choice {
            APPROVE_AND_AUTO_ACCEPT => {
                self.approve(Mode::AcceptEdits, APPROVE_AND_AUTO_ACCEPT, ctx)
            }
            APPROVE_BUT_ASK => self.approve(Mode::Ask, APPROVE_BUT_ASK, ctx),
            APPROVE_FULLY_AUTONOMOUS => self.approve(Mode::Yolo, APPROVE_FULLY_AUTONOMOUS, ctx),
            KEEP_PLANNING => {
                let feedback = ui
                    .request(
                        UiSource::Host,
                        UiRequest::Input {
                            title: "What should change in the plan?".to_string(),
                            placeholder: Some(
                                "e.g. start with the parser, not the CLI".to_string(),
                            ),
                            timeout_ms: None,
                        },
                    )
                    .await
                    .text()
                    .map(|t| t.trim().to_string())
                    .unwrap_or_default();
                if feedback.is_empty() {
                    Ok(format!(
                        "Decision: keep_planning (choice: \"{KEEP_PLANNING}\", source: plan_approved). \
                         Plan not approved. Still in plan mode; the user wants changes but said \
                         nothing — ask what to change, revise with `{}`, and resubmit.",
                        WRITE_PLAN_TOOL
                    ))
                } else {
                    Ok(format!(
                        "Decision: keep_planning (choice: \"{KEEP_PLANNING}\", source: plan_approved). \
                         Plan not approved. Still in plan mode. The user's feedback: {feedback}"
                    ))
                }
            }
            // An answer outside the offered set changes nothing: a surprising
            // value must never flip a mode.
            other => Ok(format!(
                "Decision: unrecognised (choice: \"{other}\", source: plan_approved). \
                 Unrecognised answer '{other}'. Still in plan mode."
            )),
        }
    }
}

impl ExitPlanModeTool {
    /// Apply the user's approval: the mode flips, and the model is told which
    /// mode is now judging it.
    ///
    /// The flip goes to `state` rather than the plugin's announcing setter, so
    /// the transition is reported here instead — through the run's event sink,
    /// which is the only channel a tool has. The sink is optional (a direct
    /// `execute` in a test has none) and the announcement is a pure side
    /// channel: an approval must still succeed without it.
    ///
    /// `choice` is the exact option the user picked (one of the `APPROVE_*`
    /// constants); it goes into the tool result so the approval decision lands
    /// in the store as structured text alongside `mode`/`previous`/`source`.
    fn approve(
        &self,
        mode: Mode,
        choice: &str,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let previous = self.state.mode();
        self.state.set_mode(mode);
        if let Some(sink) = ctx.event_sink.as_ref() {
            sink.emit_custom(
                MODE_CHANGED_EVENT_KIND,
                serde_json::json!({
                    "mode": mode.as_str(),
                    "previous": previous.as_str(),
                    "source": MODE_CHANGE_SOURCE_PLAN_APPROVED,
                }),
            );
        }
        Ok(format!(
            "Plan approved by user (choice: \"{choice}\" -> {}, prev: {}, source: {}). \
             Mode is now {}. Calls are judged by it from the next one on.",
            mode.as_str(),
            previous.as_str(),
            MODE_CHANGE_SOURCE_PLAN_APPROVED,
            mode.as_str()
        ))
    }
}

impl CorePlugin for ExitPlanModeTool {
    fn id(&self) -> &str {
        EXIT_PLAN_MODE_TOOL
    }
}

#[async_trait]
impl ToolPlugin for ExitPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        AgentTool::definition(self)
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String> {
        AgentTool::execute(self, args, ctx)
            .await
            .map(CallToolResult::success_text)
    }
}

impl Prepared for ModePlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn prepare<'a>(
        &'a self,
        env: &'a RunEnv,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.bind_session(env.workspace.clone(), &env.session_id);
            Ok(())
        })
    }
}

impl PromptSection for ModePlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn render(&self) -> Option<String> {
        Some(
            "### Approval mode\n             Tool calls are judged by the mode the user chose (plan / ask / acceptEdits / yolo); \
             the user can change it at any time. In plan mode every write is refused: record the \
             plan with `write_plan` (the full plan, markdown — it replaces the previous one) and \
             submit it with `exit_plan_mode`; the user's approval is the only way out. A change \
             that fits in one sentence does not need a plan."
                .to_string(),
        )
    }
}
