//! What a call *is*, not what it may do.
//!
//! The judgement used to live here — the tier, the approval modes, the
//! remembered rules — and with it a product opinion the kernel had no business
//! holding. It has moved to the host's side of the boundary: a [`ToolGate`]
//! decides, this module only describes. What remains is vocabulary both sides
//! need, which is why it is still in the kernel:
//!
//! * [`ToolEffect`] — what a call can do to the world. The guard's input, the
//!   gate's input, and the word on the wire.
//! * [`Caller`] — who asked, so an approval can never be presented as
//!   something it was not.
//! * [`describe_call`] — the one-line rendering of what is being authorised,
//!   shared by every dialog that names it.
//!
//! [`ToolGate`]: crate::types::gate::ToolGate

use serde::{Deserialize, Serialize};

/// What a tool call can do to the world.
///
/// Classification is by tool name, so an unregistered name is [`ToolEffect::Write`]
/// rather than "unknown" — an unrecognised tool must never be assumed harmless.
///
/// The serialized spellings (`read`/`write`/`exec`/`plan_artifact`/`other`) are
/// a wire contract:
/// they are what a remote tool declares its effect as, and what the trace
/// records. `thunder-agent-runtime`'s `parse_effect` accepts the same three
/// meaningful words, so a trace and a remote declaration agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolEffect {
    /// Cannot change anything outside the read-only tools' own scratch files.
    Read,
    /// Mutates the workspace.
    Write,
    /// Runs a shell command.
    Exec,
    /// Writes the plan artifact — the one file a plan-mode session may write.
    ///
    /// Exempt by *name*, not by effect: a gate that exempts this effect trusts
    /// every tool's self-declaration, so the tools that may carry it are pinned
    /// by the gate itself. The effect exists so the trace and the wire can say
    /// what the call was.
    #[serde(rename = "plan_artifact")]
    PlanArtifact,
    /// Unknown tool: treated exactly like [`ToolEffect::Write`].
    Other,
}

impl ToolEffect {
    /// Classify by tool name.
    ///
    /// Plugin, MCP and skill tools default to [`ToolEffect::Other`] (which behaves
    /// like `Write`) unless their name indicates command execution semantics, in which
    /// case they are [`ToolEffect::Exec`]: an unclassified terminal tool is not a
    /// way around asking about a shell.
    pub fn of(tool: &str) -> ToolEffect {
        let lower = tool.to_ascii_lowercase();
        match lower.as_str() {
            "read_file" | "grep" | "find" | "ls" | "list_dir" | "read" | "glob" => ToolEffect::Read,
            "bash" | "shell" | "powershell" => ToolEffect::Exec,
            "write_file" | "edit" | "write" | "apply_patch" | "notebook_edit" => ToolEffect::Write,
            // Reading the plan back is a read; writing it is the one plan-mode
            // write the wire has a word for.
            "exit_plan_mode" => ToolEffect::Read,
            "write_plan" => ToolEffect::PlanArtifact,
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

/// A short, human-readable summary of what is being authorised.
///
/// One place so the wording cannot drift between the dialog that asks and the
/// refusal that explains.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_tools_are_not_assumed_harmless() {
        assert_eq!(ToolEffect::of("some_plugin_tool"), ToolEffect::Other);
        assert_eq!(ToolEffect::of("apply_patch"), ToolEffect::Write);
        assert_eq!(ToolEffect::of("read_file"), ToolEffect::Read);
        assert_eq!(ToolEffect::of("bash"), ToolEffect::Exec);
        assert_eq!(ToolEffect::of("write_plan"), ToolEffect::PlanArtifact);
        assert_eq!(ToolEffect::of("exit_plan_mode"), ToolEffect::Read);
        // Execution-oriented external tools are exec, not write-shaped.
        assert_eq!(ToolEffect::of("mcp_terminal_exec"), ToolEffect::Exec);
        assert_eq!(ToolEffect::of("custom_shell_command"), ToolEffect::Exec);
    }

    #[test]
    fn the_wire_spellings_are_the_three_meaningful_words() {
        assert_eq!(
            serde_json::to_string(&ToolEffect::Read).unwrap(),
            r#""read""#
        );
        assert_eq!(
            serde_json::to_string(&ToolEffect::Exec).unwrap(),
            r#""exec""#
        );
        assert_eq!(
            serde_json::to_string(&ToolEffect::PlanArtifact).unwrap(),
            r#""plan_artifact""#
        );
    }
}