#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCategory {
    General,
    Config,
    SkillsAndMcp,
    Session,
}

impl CommandCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Config => "Configuration",
            Self::SkillsAndMcp => "Skills & MCP",
            Self::Session => "Session",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args_hint: &'static str,
    pub description: &'static str,
    pub category: CommandCategory,
    /// Fixed candidate values for the command's **first** argument, in the order
    /// they should be cycled. Empty for commands whose first argument is free
    /// text (a path, an id, a model name).
    pub arg_options: &'static [&'static str],
}

impl SlashCommand {
    pub const fn new(
        name: &'static str,
        aliases: &'static [&'static str],
        args_hint: &'static str,
        description: &'static str,
        category: CommandCategory,
    ) -> Self {
        Self {
            name,
            aliases,
            args_hint,
            description,
            category,
            arg_options: &[],
        }
    }

    /// Declare the first argument's candidate values. Kept separate from `new`
    /// so the 27-command table stays readable: only the commands that actually
    /// have a fixed value set pay for the extra line.
    pub const fn with_args(mut self, options: &'static [&'static str]) -> Self {
        self.arg_options = options;
        self
    }

    /// Check if a given command token matches this command or its aliases.
    pub fn matches(&self, token: &str) -> bool {
        let clean = token.trim_start_matches('/');
        if self.name.eq_ignore_ascii_case(clean) {
            return true;
        }
        self.aliases.iter().any(|a| a.eq_ignore_ascii_case(clean))
    }
}

pub static ALL_COMMANDS: &[SlashCommand] = &[
    SlashCommand::new(
        "resume",
        &["load_session", "switch", "sessions"],
        "[session_id | #]",
        "List all previous conversation sessions or resume by ID/index",
        CommandCategory::Session,
    ),
    SlashCommand::new(
        "help",
        &["?"],
        "",
        "Show interactive slash commands and keyboard shortcuts reference",
        CommandCategory::General,
    ),
    SlashCommand::new(
        "model",
        &["m"],
        "[provider/model]",
        "View current LLM model or switch to a new model (e.g., gpt-4o, claude-3-7-sonnet)",
        CommandCategory::Config,
    ),
    SlashCommand::new(
        "mode",
        &["topology"],
        "[auto | single]",
        "Switch execution mode (plugin host or direct single agent)",
        CommandCategory::Config,
    )
    .with_args(&["auto", "single"]),
    SlashCommand::new(
        "think",
        &["thinking"],
        "[off | low | medium | high]",
        "View or set the reasoning effort bound to this conversation",
        CommandCategory::Config,
    )
    .with_args(&["off", "low", "medium", "high"]),
    SlashCommand::new(
        "permission",
        &["perm"],
        "[read | write | bash]",
        "View or set the tool capability tier",
        CommandCategory::Config,
    )
    .with_args(&["read", "write", "bash"]),
    SlashCommand::new(
        "roots",
        &["root", "shared"],
        "[add <path> | remove <p|#> | clear]",
        "Grant or revoke extra workspace roots (multi-repo tasks)",
        CommandCategory::Config,
    )
    .with_args(&["add", "remove", "clear"]),
    SlashCommand::new(
        "ask",
        &["ask_user"],
        "[on | off]",
        "Mount/unmount the ask_user_question tool ",
        CommandCategory::Config,
    )
    .with_args(&["on", "off"]),
    SlashCommand::new(
        "pause",
        &["hold"],
        "",
        "Cooperatively pause the running agent at the next tool boundary",
        CommandCategory::General,
    ),
    SlashCommand::new(
        "unpause",
        &["go", "continue"],
        "",
        "Resume a paused agent run",
        CommandCategory::General,
    ),
    SlashCommand::new(
        "image",
        &["attach", "img"],
        "<path> [path...]",
        "Attach local image file(s) to the next prompt (png/jpeg/webp/gif)",
        CommandCategory::General,
    ),
    SlashCommand::new(
        "skills",
        &["skill", "sk"],
        "[attach | load <name> | show <name> | off]",
        "Attach a skill handler, show a playbook, or detach the active skill",
        CommandCategory::SkillsAndMcp,
    )
    .with_args(&["off", "load", "show", "scan"]),
    SlashCommand::new(
        "mcp",
        &["server", "tools"],
        "[list | servers | reload]",
        "Inspect connected MCP servers, discover and execute remote tools",
        CommandCategory::SkillsAndMcp,
    )
    .with_args(&["list", "servers", "reload"]),
    SlashCommand::new(
        "trace",
        &["traces"],
        "[list | <task_id>]",
        "Inspect execution traces recorded for this session's runs",
        CommandCategory::Session,
    )
    .with_args(&["list"]),
    SlashCommand::new(
        "rename",
        &[],
        "[name | --auto]",
        "Rename this session, or regenerate its name with `--auto`",
        CommandCategory::Session,
    ),
    SlashCommand::new(
        "clear",
        &["new", "reset"],
        "",
        "Clear current conversation messages and start a fresh session",
        CommandCategory::Session,
    ),
    SlashCommand::new(
        "config",
        &["settings", "cfg"],
        "[key] [value]",
        "View or modify runtime configuration (temperature, max_turns, timeout)",
        CommandCategory::Config,
    ),
    SlashCommand::new(
        "compact",
        &["prune", "compress"],
        "",
        "Compact conversation history and prune context tokens",
        CommandCategory::Session,
    ),
    SlashCommand::new(
        "stats",
        &["cost", "tokens", "usage"],
        "",
        "Display dialogue turns, token estimations, and tool execution stats",
        CommandCategory::Session,
    ),
    SlashCommand::new(
        "workspace",
        &["cwd", "dir"],
        "[path]",
        "View or change active workspace working directory",
        CommandCategory::Config,
    ),
    SlashCommand::new(
        "export",
        &["save", "dump"],
        "[path]",
        "Export the current multi-turn conversation into a Markdown file",
        CommandCategory::Session,
    ),
    SlashCommand::new(
        "preview",
        &["md", "rich"],
        "[on | off]",
        "Toggle the rendered Markdown preview against the raw source",
        CommandCategory::General,
    )
    .with_args(&["on", "off"]),
    SlashCommand::new(
        "links",
        &["urls", "files"],
        "",
        "List every link and file path in this session; Enter reveals or opens it",
        CommandCategory::Session,
    ),
    SlashCommand::new(
        "queue",
        &["queued", "pending"],
        "[clear | all | one]",
        "Inspect or manage messages queued into the running agent",
        CommandCategory::General,
    )
    .with_args(&["clear", "all", "one"]),
    SlashCommand::new(
        "metrics",
        &["meters"],
        "[on | off]",
        "Show or hide the token, cache and speed bar above the prompt",
        CommandCategory::General,
    )
    .with_args(&["on", "off"]),
    SlashCommand::new(
        "timeline",
        &["rail"],
        "[on | off]",
        "Show or hide the turn rail floating on the right of the transcript",
        CommandCategory::General,
    )
    .with_args(&["on", "off"]),
    SlashCommand::new(
        "details",
        &["verbose"],
        "[on | off]",
        "Expand or collapse thinking and tool details in the transcript",
        CommandCategory::General,
    )
    .with_args(&["on", "off"]),
    SlashCommand::new(
        "health",
        &["doctor", "status"],
        "",
        "Run agent and environment health checks",
        CommandCategory::General,
    ),
    SlashCommand::new(
        "quit",
        &["exit", "q"],
        "",
        "Cleanly exit Thunder TUI",
        CommandCategory::General,
    ),
];

/// Find commands matching the input prefix.
pub fn filter_commands(input: &str) -> Vec<&'static SlashCommand> {
    if !input.starts_with('/') {
        return Vec::new();
    }

    let search = input.trim_start_matches('/').to_lowercase();
    let search_token = search.split_whitespace().next().unwrap_or("");

    ALL_COMMANDS
        .iter()
        .filter(|cmd| {
            if search_token.is_empty() {
                return true;
            }
            if cmd.name.starts_with(search_token) {
                return true;
            }
            cmd.aliases.iter().any(|a| a.starts_with(search_token))
        })
        .collect()
}

/// Resolve a typed command token (name or alias, with or without the leading
/// slash) to its command definition.
pub fn lookup_command(token: &str) -> Option<&'static SlashCommand> {
    ALL_COMMANDS.iter().find(|cmd| cmd.matches(token))
}

/// Direction of an argument cycle, so `Tab` and `Shift+Tab` stay symmetric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleDir {
    Forward,
    Backward,
}

/// The first-argument position of a command that has a fixed candidate set.
///
/// Only the **first** argument participates: `/skills load <name>` completes the
/// action word (`load`), never the name that follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgContext<'a> {
    pub command: &'static SlashCommand,
    /// The first argument exactly as typed. Empty right after `/think `.
    pub token: &'a str,
    /// Byte range of `token` inside the original input, so cycling rewrites the
    /// argument in place without disturbing the command or any later text.
    pub token_range: std::ops::Range<usize>,
}

/// Whether the caret sits in the first-argument slot of a command with fixed
/// candidates. `None` means "leave the input alone": no slash, an unknown
/// command, a command whose first argument is free text, or a command name that
/// is still being typed (no space yet).
pub fn arg_context(input: &str) -> Option<ArgContext<'_>> {
    let after_slash = input.strip_prefix('/')?;
    let cmd_end = after_slash.find(char::is_whitespace)? + 1;
    let command = lookup_command(&after_slash[..cmd_end - 1])?;
    if command.arg_options.is_empty() {
        return None;
    }

    // Collapse any extra spacing after the command: the argument starts at the
    // first non-blank character, and cycling writes there.
    let rest = &input[cmd_end..];
    let lead = rest.len() - rest.trim_start().len();
    let token = &rest[lead..];
    let token_len = token.find(char::is_whitespace).unwrap_or(token.len());
    let start = cmd_end + lead;
    Some(ArgContext {
        command,
        token: &rest[lead..lead + token_len],
        token_range: start..start + token_len,
    })
}

/// Candidate values to render for the current token: every value while the
/// argument is empty, otherwise those the typed prefix still matches.
pub fn arg_candidates(command: &SlashCommand, token: &str) -> Vec<&'static str> {
    if token.is_empty() {
        return command.arg_options.to_vec();
    }
    command
        .arg_options
        .iter()
        .copied()
        .filter(|option| starts_with_ignore_case(option, token))
        .collect()
}

/// Which rendered candidate row is the one currently in the input, so the popup
/// highlight always equals what `Enter` would run.
pub fn arg_highlight(command: &SlashCommand, token: &str) -> usize {
    if token.is_empty() {
        return 0;
    }
    arg_candidates(command, token)
        .iter()
        .position(|option| option.eq_ignore_ascii_case(token))
        .unwrap_or(0)
}

/// Rewrite the first argument to the next (or previous) candidate.
///
/// - empty argument: first value forward, last value backward
/// - an exact value: step through the full list, wrapping
/// - a partial prefix: fill in the first (last, backward) matching value, so
///   the next press steps from a real value instead of stalling on one match
///
/// `None` when there is no argument slot here or nothing matches the prefix;
/// callers fall back to their previous key handling.
pub fn arg_cycle(input: &str, dir: CycleDir) -> Option<String> {
    let ctx = arg_context(input)?;
    let options = ctx.command.arg_options;

    let index = if ctx.token.is_empty() {
        match dir {
            CycleDir::Forward => 0,
            CycleDir::Backward => options.len() - 1,
        }
    } else if let Some(current) = options
        .iter()
        .position(|option| option.eq_ignore_ascii_case(ctx.token))
    {
        let len = options.len();
        match dir {
            CycleDir::Forward => (current + 1) % len,
            CycleDir::Backward => (current + len - 1) % len,
        }
    } else {
        let matches: Vec<usize> = options
            .iter()
            .enumerate()
            .filter(|(_, option)| starts_with_ignore_case(option, ctx.token))
            .map(|(idx, _)| idx)
            .collect();
        match dir {
            CycleDir::Forward => *matches.first()?,
            CycleDir::Backward => *matches.last()?,
        }
    };

    let mut out = String::with_capacity(input.len() + options[index].len());
    out.push_str(&input[..ctx.token_range.start]);
    out.push_str(options[index]);
    out.push_str(&input[ctx.token_range.end..]);
    Some(out)
}

fn starts_with_ignore_case(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix)
}
