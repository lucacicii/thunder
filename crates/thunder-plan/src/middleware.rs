use crate::read_only::call_is_read_only;
use async_trait::async_trait;
use serde_json::Value;
use thunder_core::{MiddlewareVerdict, ToolMiddleware};
use thunder_protocol::ExecutionContext;

/// Plan Mode authorization middleware.
///
/// In Plan Mode:
/// - `write_plan` and `exit_plan_mode` are always allowed (exempted from write restrictions);
/// - Read-only tools (`read_file`, `grep`, `find`, `ls`, `thunder__discover_tools`) are allowed;
/// - Shell commands are statically checked via `call_is_read_only` (only pure read commands like `git status`, `rg`, `cat` are allowed);
/// - Any mutating tools (`write_file`, `edit_block`, scripts without read exemption) are strictly rejected with code 403.
#[derive(Clone, Default)]
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

        // 1. Plan mode tools are explicitly exempted
        if matches!(logical, "write_plan" | "exit_plan_mode") {
            return MiddlewareVerdict::Allow;
        }

        // 2. Read-only tools are allowed
        if matches!(
            logical,
            "read_file" | "grep" | "find" | "ls" | "thunder__discover_tools"
        ) {
            return MiddlewareVerdict::Allow;
        }

        // 3. Shell execution is allowed only when statically proven 100% read-only
        if matches!(logical, "bash" | "sh" | "zsh") {
            if call_is_read_only(arguments) {
                return MiddlewareVerdict::Allow;
            }
            let cmd = arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            return MiddlewareVerdict::Reject {
                reason: format!(
                    "Plan mode refuses non-read-only shell command: '{cmd}'. Only read commands (git status/diff, grep, ls, cat) are permitted while planning."
                ),
            };
        }

        // 4. All other tools (file writes, edits, mutations) are refused
        MiddlewareVerdict::Reject {
            reason: format!(
                "Plan mode refuses tool '{tool_name}'. You are in planning mode and must not modify code or run mutating tools. Formulate your proposal with `write_plan` instead."
            ),
        }
    }
}
