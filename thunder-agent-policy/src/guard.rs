//! The kernel's floor: the calls it refuses in every mode, without asking.
//!
//! This is not a policy and it is not configurable. Everything else in the
//! permission system is a question — *may I?* — that a mode, a rule or the user
//! answers. A handful of calls are not questions: they are the ones whose
//! damage cannot be undone by the user saying yes. Those live here, below every
//! gate, and no host can loosen them.
//!
//! **The whole list is deliberately small, and never grows by guesswork.** The
//! predecessor of this check matched *substrings* of the command line, which
//! made `rm -rf /` fire on `rm -rf /tmp/build` and on the message body of
//! `git commit -m "never run rm -rf /"` — and a refusal nobody can override is
//! the worst possible answer to a false positive. So:
//!
//! * matching is on **tokens**, not substrings: a command only counts when its
//!   verb and operands say so;
//! * the deny set is a **subset of what was already denied**, so this change can
//!   only ever remove refusals, never add one (the sole addition is a host's
//!   explicit [`Scope::Critical`] classification, which the host chooses);
//! * anything this cannot parse is *not* denied here — it becomes a question at
//!   the gate, which is where an uncertain judgement belongs.
//!
//! That last point is the honest limit of this file. It is a floor for the
//! obvious forms, not a sandbox: `bash -c "$(rm -rf /)"` is not recognised, and
//! a command assembled at runtime cannot be recognised. What saves those cases is
//! that a shell command's target is never statically known, so it never earns an
//! automatic allowance either.

use crate::trace::{GuardHit, GuardKind};
use thunder_agent_types::policy::ToolEffect;
use thunder_agent_types::scope::Scope;
use serde_json::Value;

/// Verbs that run the *next* verb rather than doing work themselves.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "command", "exec", "nohup", "nice", "ionice", "time", "stdbuf", "xargs",
];

/// Argument keys a shell-ish tool might carry its command in.
///
/// Shared with [`read_only`](super::read_only), which asks the opposite question
/// about the same string: a command the guard would not refuse is one plan mode
/// may still be able to prove harmless.
pub(crate) const COMMAND_KEYS: &[&str] = &["command", "cmd", "script"];

/// Keys an argument might name a filesystem target in, for the guard's reports.
const TARGET_KEYS: &[&str] = &[
    "path",
    "file_path",
    "filePath",
    "target",
    "cwd",
    "destination",
];

/// The kernel's unconditional refusals.
#[derive(Debug, Default, Clone, Copy)]
pub struct Guard;

impl crate::plugin::Plugin for Guard {
    fn id(&self) -> &str {
        "kernel_guard"
    }

    fn description(&self) -> &str {
        "Unconditional security floor: blocks destructive commands and critical path writes."
    }
}

#[async_trait::async_trait]
impl crate::plugin::InterceptorPlugin for Guard {
    fn priority(&self) -> i32 {
        crate::plugin::priority::CRITICAL_GUARD
    }

    async fn pre_execute(
        &self,
        tool: &str,
        args: &serde_json::Value,
        ctx: &crate::tool_policy::ToolExecutionContext,
    ) -> Result<crate::plugin::PreExecutionVerdict, String> {
        let effect = ctx.effect.unwrap_or_else(|| ToolEffect::of(tool));
        if let Some(hit) = Guard::check(tool, args, effect, Scope::Unknown) {
            return Ok(crate::plugin::PreExecutionVerdict::Deny {
                reason: format!(
                    "Refused by kernel security guard: {:?} on pattern '{}'",
                    hit.kind, hit.pattern
                ),
            });
        }
        Ok(crate::plugin::PreExecutionVerdict::Allow)
    }
}

impl Guard {
    /// Refuse this call, or let it through to the gate.
    ///
    /// `scope` is the host's classification: a [`Scope::Critical`] target is the
    /// host saying "do not even ask", and is the one way a host can add a refusal
    /// this file does not know about.
    pub fn check(
        tool: &str,
        arguments: &Value,
        effect: ToolEffect,
        scope: Scope,
    ) -> Option<GuardHit> {
        if scope == Scope::Critical {
            return Some(GuardHit {
                kind: GuardKind::CriticalPath,
                pattern: "critical path".to_string(),
                detail: target_of(arguments).unwrap_or_else(|| tool.to_string()),
            });
        }

        if !is_shell_like(tool, effect) {
            return None;
        }
        let command = COMMAND_KEYS
            .iter()
            .find_map(|key| arguments.get(*key).and_then(Value::as_str))?;
        let (pattern, detail) = dangerous_command(command)?;
        Some(GuardHit {
            kind: GuardKind::DangerousCommand,
            pattern: pattern.to_string(),
            detail,
        })
    }
}

/// Whether this tool runs a shell command, and so is subject to the command list.
///
/// Keyed on the declared effect rather than the name, so a plugin or MCP tool
/// that honestly declares `Exec` is covered without the kernel having to know
/// its name. The name check stays as a second net for tools that declare
/// nothing: [`ToolEffect::of`] already classifies those by name, but a tool with
/// a misleading declaration should not become a way around the floor.
fn is_shell_like(tool: &str, effect: ToolEffect) -> bool {
    if effect == ToolEffect::Exec {
        return true;
    }
    let lower = tool.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "bash" | "shell" | "sh" | "zsh" | "powershell"
    )
}

/// The dangerous-command list, matched on tokens.
///
/// `Some((pattern, detail))` where `pattern` is the rule that matched and
/// `detail` is the part of the command that tripped it.
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

/// Inspect one simple command (a `;`/`|`/`&&`-separated piece).
fn inspect(words: &[String]) -> Option<(&'static str, String)> {
    let mut verb: Option<&str> = None;
    let mut flags: Vec<&str> = Vec::new();
    let mut operands: Vec<&str> = Vec::new();

    for word in words {
        if verb.is_none() {
            // Leading assignments, wrapper flags, `nice`'s numeric level and the
            // wrapper verbs themselves all sit before the verb proper.
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
        // Both flags, or the command is not the unrecoverable form: `rm -r x`
        // asks, and `rm -f x` asks. Only "no prompt, no confirmation, and a
        // target that is the whole filesystem" is out of the question.
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

/// The `:(){ :|:& };:` fork bomb, in any spacing.
///
/// The one rule that stays a text match, because it has no meaningful tokens —
/// and because it is a single distinctive form that nothing else resembles.
fn fork_bomb(command: &str) -> Option<String> {
    let squeezed: String = command.chars().filter(|c| !c.is_whitespace()).collect();
    squeezed
        .contains(":(){:|:&};:")
        .then(|| ":(){ :|:& };:".to_string())
}

/// Split a command line into simple commands of words.
///
/// Quotes are tracked, which is the entire reason this exists: `git commit -m
/// "never run rm -rf /"` is one word to `git`, not a call to `rm`. Command
/// separators (`;`, `|`, `&`, newlines, subshell and brace brackets, backticks)
/// start a new simple command; redirection characters are word breaks, which is
/// enough here because a redirection can never turn `echo` into `rm`.
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

/// Whether a raw operand names the filesystem root (`/`, `//`, `/*`).
fn is_filesystem_root(target: &str) -> bool {
    let trimmed = target.trim().trim_matches('/');
    trimmed.is_empty() || trimmed == "*"
}

/// The last path component, so `/bin/rm` is still `rm`.
fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// `VAR=value`, which is an assignment wherever it appears.
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

/// The target an argument names, for the guard's reports.
fn target_of(arguments: &Value) -> Option<String> {
    TARGET_KEYS
        .iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deny(command: &str) -> Option<(&'static str, String)> {
        dangerous_command(command)
    }

    #[test]
    fn refuses_the_unrecoverable_rm() {
        for command in [
            "rm -rf /",
            "rm -rf /*",
            "rm -rf //",
            "rm -rf -- /",
            "rm -r -f /",
            "sudo rm -rf /",
            "/bin/rm -rf /",
            "cd /tmp && rm -rf /",
            "echo hi; rm -rf /",
            "rm  -rf   /",
        ] {
            assert_eq!(
                deny(command).map(|(p, _)| p),
                Some("rm -rf /"),
                "{command} must be refused"
            );
        }
    }

    #[test]
    fn refuses_mkfs_anywhere() {
        assert_eq!(deny("mkfs /dev/sda1").map(|(p, _)| p), Some("mkfs"));
        assert_eq!(
            deny("sudo mkfs.ext4 /dev/sda1").map(|(p, _)| p),
            Some("mkfs")
        );
    }

    #[test]
    fn refuses_dd_with_an_input_file() {
        assert_eq!(
            deny("dd if=/dev/zero of=/dev/sda").map(|(p, _)| p),
            Some("dd if=")
        );
    }

    #[test]
    fn refuses_the_fork_bomb_in_any_spacing() {
        for command in [":(){ :|:& };:", ":(){:|:&};:", ":() { :|:& };:"] {
            assert_eq!(
                deny(command).map(|(p, _)| p),
                Some("fork bomb"),
                "{command} must be refused"
            );
        }
    }

    /// The refusals that made the old check unbearable: each of these is a
    /// *substring* match away from being blocked, and none of them is a call to
    /// the thing it mentions.
    #[test]
    fn does_not_refuse_a_mention_of_a_dangerous_command() {
        for command in [
            r#"git commit -m "never run rm -rf /""#,
            r#"echo "error: wrote > /tmp/log""#,
            r#"echo "rm -rf /" > /tmp/howto.md"#,
            r#"grep -rn "mkfs" docs/"#,
            r#"echo dd if= > /tmp/log"#,
            r#"python3 -c "import os; os.system('cat /etc/hosts')""#,
            "rm -rf /tmp/build",
            "rm -rf build/target",
            "rm -f leftover",
            "rm -r old_dir",
            "rm -rf $BUILD_DIR",
            "echo done > \"$LOG\"",
            "cd \"$BUILD_DIR\" && touch marker",
            "pushd src && cargo test && popd && touch done",
            "find . | xargs rm -f",
            "cargo clean",
        ] {
            assert_eq!(deny(command), None, "{command} must not be refused");
        }
    }

    #[test]
    fn a_critical_target_is_refused_whatever_the_tool() {
        let hit = Guard::check(
            "write_file",
            &json!({ "path": "/ws/.git/HEAD", "content": "x" }),
            ToolEffect::Write,
            Scope::Critical,
        )
        .expect("critical targets are refused");
        assert_eq!(hit.kind, GuardKind::CriticalPath);
        assert_eq!(hit.detail, "/ws/.git/HEAD");
    }

    #[test]
    fn a_scope_cannot_rescue_a_dangerous_command() {
        // The host saying "in scope" must not unlock `rm -rf /`: this is the
        // ordering the wrapper guarantees, pinned here at the guard's own level.
        let hit = Guard::check(
            "bash",
            &json!({ "command": "rm -rf /" }),
            ToolEffect::Exec,
            Scope::InScope,
        )
        .expect("refused");
        assert_eq!(hit.kind, GuardKind::DangerousCommand);
        assert_eq!(hit.pattern, "rm -rf /");
    }

    #[test]
    fn an_exec_tool_is_covered_even_under_an_unfamiliar_name() {
        let hit = Guard::check(
            "deploy_runner",
            &json!({ "command": "mkfs /dev/disk2" }),
            ToolEffect::Exec,
            Scope::Unknown,
        );
        assert_eq!(hit.map(|h| h.pattern), Some("mkfs".to_string()));
    }

    #[test]
    fn a_non_exec_tool_is_left_alone() {
        // `bash` in a *string* argument of some other tool is not a call to it.
        assert!(Guard::check(
            "save_snippet",
            &json!({ "command": "rm -rf /" }),
            ToolEffect::Write,
            Scope::Unknown,
        )
        .is_none());
    }

    #[test]
    fn empty_and_unparseable_commands_are_not_refused() {
        assert_eq!(deny(""), None);
        assert_eq!(deny("   "), None);
        assert_eq!(deny("rm"), None);
        assert_eq!(deny("rm -rf"), None);
    }

    #[test]
    fn assignments_before_the_verb_do_not_hide_it() {
        assert_eq!(deny("FOO=1 rm -rf /").map(|(p, _)| p), Some("rm -rf /"));
        assert_eq!(
            deny("nice -n 10 rm -rf /").map(|(p, _)| p),
            Some("rm -rf /")
        );
    }
}