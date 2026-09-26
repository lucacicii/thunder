
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
        }
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
    ),
    SlashCommand::new(
        "skills",
        &["skill", "sk"],
        "[attach | load <name> | show <name> | off]",
        "Attach a skill handler, show a playbook, or detach the active skill",
        CommandCategory::SkillsAndMcp,
    ),
    SlashCommand::new(
        "mcp",
        &["server", "tools"],
        "[list | servers | reload]",
        "Inspect connected MCP servers, discover and execute remote tools",
        CommandCategory::SkillsAndMcp,
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
