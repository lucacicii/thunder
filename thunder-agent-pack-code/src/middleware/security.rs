use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thunder_agent_loop::tools::middleware::telemetry::SystemNotice;
use thunder_agent_loop::tools::middleware::{ToolHandler, ToolMiddleware};
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::tool::{ToolExecutionContext, ToolExecutionResult};

/// Security Guard Middleware.
///
/// Enforces workspace jail boundaries (preventing path traversal attacks like `../../etc/passwd`),
/// and blocks dangerous/destructive shell commands before execution.
///
/// The jail is multi-root: the primary workspace plus any extra roots the host
/// granted (e.g. repositories referenced by the task). All roots share the same
/// read/write standing; relative paths always resolve against the primary root.
///
/// # Shell command inspection (best-effort — NOT a sandbox)
///
/// `bash` is a general-purpose interpreter, so this layer cannot be a complete
/// jail; it blocks the high-confidence escapes and fails closed where it
/// cannot tell. Covered:
///
/// * redirections (`>`, `>>`, `2>`, `&>`) and operands of `rm`/`cp`/`mv`/`tee`/
///   `touch`/`mkdir`/`chmod`/`rsync`/`sed -i`/`dd of=`/`curl -o`/`wget -O`, …;
/// * `cd` / `pushd` / `popd` tracking: relative targets are judged in the
///   directory the command actually runs in, so `cd /outside && touch x` fails;
/// * command boundaries (`;`, `&&`, `|`, newline, `( )`, `$( )`, backticks),
///   wrapper verbs (`sudo`, `env`, `FOO=1 cmd`, `/bin/rm`) and `bash -c` / `eval`;
/// * inline interpreter scripts (`python -c`, `python - <<EOF`, `node -e`,
///   `perl -pi`, `ruby -e`): a script containing a file-writing construct must
///   run from inside a root and may not name a path outside the roots;
/// * targets that start with a substitution (`$VAR`, `` `cmd` ``) are refused:
///   their real location is unknowable. Write a literal path instead.
///
/// Not covered (use an OS-level sandbox if you need a hard guarantee): scripts
/// run from a file (`python build.py`), `find -exec`, `git`/`tar`/`unzip` with
/// destination flags, heredoc bodies fed to a shell, and anything assembled at
/// runtime. Reads through bash are not restricted.
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
const WRITE_LAST_OPERAND: &[&str] = &["cp", "mv", "ln", "install", "rsync"];
/// Commands that take one control operand first (mode / owner), then targets.
const WRITE_AFTER_FIRST_OPERAND: &[&str] = &["chmod", "chown"];
/// Commands that run another command: the next word is the real verb.
const WRAPPERS: &[&str] = &[
    "sudo", "env", "command", "nohup", "time", "exec", "builtin", "xargs",
];
/// Shell keywords after which a new command starts.
const SHELL_KEYWORDS: &[&str] = &[
    "then", "do", "else", "elif", "if", "while", "until", "!", "{",
];
/// Shells whose `-c` operand is a command line.
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh", "fish"];
/// Script interpreters (`python*` is matched by prefix as well).
const INTERPRETERS: &[&str] = &[
    "node",
    "nodejs",
    "deno",
    "bun",
    "perl",
    "ruby",
    "php",
    "lua",
    "Rscript",
    "osascript",
];
/// Constructs in interpreter source that write files or spawn processes.
const SCRIPT_WRITE_MARKERS: &[&str] = &[
    ".write(",
    "write_text",
    "write_bytes",
    "writeFile",
    "appendFile",
    "createWriteStream",
    "writeSync",
    "File.write",
    "IO.write",
    "FileUtils",
    "shutil.",
    "os.remove",
    "os.unlink",
    "os.rename",
    "os.replace",
    "os.makedirs",
    "os.mkdir",
    "os.rmdir",
    ".unlink(",
    ".mkdir(",
    ".touch(",
    ".rmdir(",
    "unlinkSync",
    "rmSync",
    "renameSync",
    "mkdirSync",
    "copyFile",
    "fs.rm",
    "fs.unlink",
    "fs.rename",
    "fs.mkdir",
    "fs.cp",
    "subprocess",
    "os.system",
    "os.popen",
    "child_process",
    "execSync",
    "spawnSync",
    "system(",
    "unlink(",
    "unlink ",
    "File.delete",
    "File.rename",
    "Deno.write",
    "Deno.remove",
    "Bun.write",
    "file_put_contents",
    "fwrite",
];
/// File modes that make an `open(` call a write.
const SCRIPT_WRITE_MODES: &[&str] = &[
    "\"w\"", "'w'", "\"wb\"", "'wb'", "\"w+\"", "'w+'", "\"wt\"", "'wt'", "\"a\"", "'a'", "\"ab\"",
    "'ab'", "\"a+\"", "'a+'", "\"at\"", "'at'", "\"x\"", "'x'", "\"r+\"", "'r+'", "\"rb+\"",
    "'rb+'", "\"r+b\"", "'r+b'", "\">", "'>",
];

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
    pub fn with_extra_roots(mut self, roots: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
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
        self.allowed_roots
            .iter()
            .any(|root| normalized.starts_with(root))
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

    /// Expands a leading `~` / `$HOME` / `${HOME}` in a path-like string.
    /// Returns `None` when `HOME` is unknown.
    fn expand_home(target: &str) -> Option<String> {
        let home = std::env::var("HOME").ok()?;
        let rest = if target == "~" {
            ""
        } else if let Some(r) = target.strip_prefix("~/") {
            return Some(format!("{home}/{r}"));
        } else if let Some(r) = target.strip_prefix("${HOME}") {
            r
        } else if let Some(r) = target.strip_prefix("$HOME") {
            r
        } else {
            return Some(target.to_string());
        };
        Some(format!("{home}{rest}"))
    }

    /// Resolves the directory a `cd`/`pushd` operand leads to.
    ///
    /// Anything that cannot be known statically (`cd -`, `cd "$X"`, a relative
    /// path from an unknown directory) yields [`Dir::Unknown`]; the caller
    /// then refuses write-capable operations that depend on the cwd.
    fn resolve_cd(&self, cur: &Dir, arg: Option<&str>) -> Dir {
        let Some(arg) = arg else {
            return match Self::expand_home("~") {
                Some(h) => Dir::Known(normalize_path(&resolve_for_check(Path::new(&h)))),
                None => Dir::Unknown,
            };
        };
        let arg = arg.trim_matches(|c| c == '"' || c == '\'');
        if arg == "-" || arg.contains('`') {
            return Dir::Unknown;
        }
        let Some(expanded) = Self::expand_home(arg) else {
            return Dir::Unknown;
        };
        if expanded.contains('$') {
            return Dir::Unknown;
        }
        let path = PathBuf::from(&expanded);
        let joined = if path.is_absolute() {
            path
        } else {
            match cur {
                Dir::Known(d) => d.join(path),
                Dir::Unknown => return Dir::Unknown,
            }
        };
        Dir::Known(normalize_path(&resolve_for_check(&joined)))
    }

    /// Validates a statically-known shell write target against the jail.
    ///
    /// Relative targets resolve against `dir` — the directory the command runs
    /// in after any preceding `cd` — so `cd /outside && touch x` is judged on
    /// `/outside/x`. Fail-closed cases (all refused with a "Shell write target"
    /// message):
    ///   * the target starts with a substitution (`$VAR`, `` `cmd` ``, `$(..)`),
    ///     so its real location is unknowable here — write a literal path;
    ///   * the target is relative but the cwd is unknown (`cd "$X"`).
    ///
    /// A substitution in the middle of an otherwise literal path (`out/$n.txt`)
    /// is judged on its literal directory prefix.
    fn check_write_target(&self, target: &str, dir: &Dir) -> Result<(), String> {
        let target = target.trim_matches(|c| c == '"' || c == '\'');
        if target.is_empty() || target.starts_with('&') {
            // `2>&1`-style fd duplication, or an empty operand.
            return Ok(());
        }
        if DEVICE_ALLOWLIST
            .iter()
            .any(|dev| target == *dev || target.starts_with(&format!("{}/fd/", dev)))
        {
            return Ok(());
        }

        let expanded = match Self::expand_home(target) {
            Some(e) => e,
            None => return Ok(()), // no HOME: nothing sensible to compare
        };

        // Cut at the first substitution; judge the literal directory before it.
        let subst = expanded.find(['$', '`']);
        let literal: &str = match subst {
            None => &expanded,
            Some(0) => {
                return Err(format!(
                    "Shell write target '{}' cannot be verified statically (it starts with a shell \
                     substitution). Use a literal path inside the allowed workspace roots [{}]",
                    target,
                    self.roots_display()
                ));
            }
            Some(idx) => {
                let prefix = &expanded[..idx];
                match prefix.rfind('/') {
                    Some(slash) => &prefix[..=slash],
                    None => "",
                }
            }
        };

        let candidate = if literal.starts_with('/') {
            PathBuf::from(literal)
        } else {
            match dir {
                Dir::Known(d) => d.join(literal),
                Dir::Unknown => {
                    return Err(format!(
                        "Shell write target '{}' is relative to a working directory that cannot be \
                         determined statically (an earlier `cd` used a non-literal path). \
                         Use absolute paths inside the allowed workspace roots [{}]",
                        target,
                        self.roots_display()
                    ));
                }
            }
        };

        // Lexical check for plain relative targets (preserves symlinked
        // `node_modules`-style layouts); resolve symlinks for absolute paths
        // and anything using `..`.
        let normalized = if !literal.starts_with('/') && !literal.contains("..") {
            normalize_path(&candidate)
        } else {
            normalize_path(&resolve_for_check(&candidate))
        };
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

    /// Validates an inline interpreter script (`python3 - <<EOF`, `node -e`,
    /// `perl -pi -e`, ...). It is not parsed; instead:
    ///
    ///   1. scripts with no write-capable construct are left alone (reads are
    ///      not the jail's business);
    ///   2. a script that does write must run from a known directory inside the
    ///      jail — relative `open(p, "w")` after `cd /outside` is exactly the
    ///      escape this blocks;
    ///   3. every absolute / `~` / `..` path literal in it must be inside the jail.
    fn check_interpreter(&self, command: &str, dir: &Dir, inplace: bool) -> Result<(), String> {
        let text = clean_script_text(command);
        if !(inplace || script_writes(&text)) {
            return Ok(());
        }
        match dir {
            Dir::Known(d) if self.is_allowed(d) => {}
            Dir::Known(d) => {
                return Err(format!(
                    "Shell write target: an interpreter script that writes files runs from '{}', \
                     outside the allowed workspace roots [{}]",
                    d.display(),
                    self.roots_display()
                ));
            }
            Dir::Unknown => {
                return Err(format!(
                    "Shell write target: an interpreter script that writes files runs from a \
                     directory that cannot be determined statically (non-literal `cd`). \
                     Allowed workspace roots: [{}]",
                    self.roots_display()
                ));
            }
        }
        for lit in script_path_literals(&text) {
            self.check_write_target(&lit, dir).map_err(|e| {
                format!("{e} (found inside an interpreter script that writes files)")
            })?;
        }
        Ok(())
    }

    /// Checks a bash command (run from the primary workspace root) for
    /// forbidden destructive patterns and write targets outside the jail.
    pub fn check_command(&self, command: &str) -> Result<(), String> {
        self.check_command_in(command, &self.workspace_root)
    }

    /// Like [`check_command`](Self::check_command) but for a command whose
    /// starting directory is `start` (the tool call's `cwd`).
    pub fn check_command_in(&self, command: &str, start: &Path) -> Result<(), String> {
        let trimmed = command.trim();
        for forbidden in &self.forbidden_commands {
            if trimmed.contains(forbidden) {
                return Err(format!(
                    "Forbidden high-risk command pattern detected: '{}'",
                    forbidden
                ));
            }
        }
        let scan = self.scan_command(trimmed, Dir::Known(start.to_path_buf()));
        for (target, dir) in &scan.writes {
            self.check_write_target(target, dir)?;
        }
        for (dir, inplace) in &scan.interpreters {
            self.check_interpreter(trimmed, dir, *inplace)?;
        }
        Ok(())
    }

    /// Single pass over a command line. Collects write targets (each with the
    /// directory it will be evaluated in) and interpreter invocations; tracks
    /// `cd` / `pushd` / `popd` as it goes.
    ///
    /// Best-effort tokenization — see the type-level docs for what is and is
    /// not covered.
    fn scan_command(&self, command: &str, start: Dir) -> Scan {
        let toks = tokenize(command);
        let mut scan = Scan::default();
        let mut dir = start;
        let mut i = 0;

        let mut mode = WriteMode::None;
        // Command words (and `cd`) are only recognized in command position so
        // `echo cp /etc/x` never makes `/etc/x` a target.
        let mut command_position = true;

        while i < toks.len() {
            let (bare, tok) = match &toks[i] {
                Tok::Sep => {
                    flush_last_operand(&mut mode, &dir, &mut scan);
                    mode = WriteMode::None;
                    command_position = true;
                    i += 1;
                    continue;
                }
                Tok::Word(w) => (unquote(w), *w),
            };

            // `dd of=PATH` (and any `of=` operand) names a file it writes.
            if let Some(path) = bare.strip_prefix("of=") {
                scan.writes.push((path.to_string(), dir.clone()));
                command_position = false;
                i += 1;
                continue;
            }

            // 1) Redirection: `>`, `>>`, `2>`, `&>`, possibly fused (`>/x`).
            if let Some(embedded) = redirect_operand(bare) {
                match embedded {
                    Some(target) if !target.is_empty() => {
                        scan.writes.push((target.to_string(), dir.clone()))
                    }
                    _ => match toks.get(i + 1) {
                        Some(Tok::Word(next)) => {
                            scan.writes.push((unquote(next).to_string(), dir.clone()));
                            i += 1;
                        }
                        // `> \`cmd\``, `> >(cmd)`: the target is a substitution, whose
                        // location is unknowable here. Fail closed.
                        _ => scan.writes.push(("$(...)".to_string(), dir.clone())),
                    },
                }
                // A redirect does not consume command position (`>f cmd`).
                i += 1;
                continue;
            }

            // 2) Command words are recognized only in command position.
            if command_position {
                // Skip options of a wrapper (`sudo -u x`, `env -i`): still waiting for the verb.
                if bare.starts_with('-') && bare.len() > 1 {
                    i += 1;
                    continue;
                }
                // Prefix assignment (`FOO=1 rm x`): the verb comes next.
                if is_assignment(bare) {
                    i += 1;
                    continue;
                }
                let verb = basename(bare);

                if WRAPPERS.contains(&verb) {
                    i += 1;
                    continue; // next word is the real verb
                }
                // Shell keywords that introduce another command.
                if SHELL_KEYWORDS.contains(&verb) {
                    i += 1;
                    continue;
                }

                if verb == "cd" || verb == "pushd" {
                    let mut j = i + 1;
                    while let Some(Tok::Word(w)) = toks.get(j) {
                        if w.starts_with('-') && *w != "-" {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    let arg = match toks.get(j) {
                        Some(Tok::Word(w)) => Some(*w),
                        _ => None,
                    };
                    dir = self.resolve_cd(&dir, arg);
                    i = if arg.is_some() { j + 1 } else { j };
                    command_position = false;
                    mode = WriteMode::None;
                    continue;
                }
                if verb == "popd" {
                    dir = Dir::Unknown;
                    i += 1;
                    command_position = false;
                    mode = WriteMode::None;
                    continue;
                }
                if verb == "eval" {
                    // The operand string is itself a command line.
                    i += 1;
                    continue;
                }

                mode = if WRITE_ALL_OPERANDS.contains(&verb) {
                    WriteMode::AllOperands
                } else if WRITE_LAST_OPERAND.contains(&verb) {
                    WriteMode::LastOperand(Vec::new())
                } else if WRITE_AFTER_FIRST_OPERAND.contains(&verb) {
                    WriteMode::AfterFirst(None)
                } else if verb == "sed" {
                    // Only `sed -i` writes; look at this segment's options.
                    let in_place = toks[i + 1..]
                        .iter()
                        .take_while(|t| matches!(t, Tok::Word(_)))
                        .any(|t| match t {
                            Tok::Word(w) => w.starts_with("-i") || w.starts_with("--in-place"),
                            Tok::Sep => false,
                        });
                    if in_place {
                        WriteMode::AllOperands
                    } else {
                        WriteMode::None
                    }
                } else if verb == "curl" {
                    WriteMode::OutputFlag(&["-o", "--output"], &["-O", "--remote-name"])
                } else if verb == "wget" {
                    WriteMode::OutputFlag(
                        &["-O", "--output-document", "-P", "--directory-prefix"],
                        &[],
                    )
                } else if SHELLS.contains(&verb) {
                    WriteMode::Shell
                } else if is_interpreter(verb) {
                    scan.interpreters.push((dir.clone(), false));
                    WriteMode::Interpreter(
                        scan.interpreters.len() - 1,
                        verb == "perl" || verb == "ruby",
                    )
                } else {
                    WriteMode::None
                };
                command_position = false;
                i += 1;
                continue;
            }

            // 3) Options / operands of the active command.
            if tok.starts_with('-') && tok.len() > 1 {
                match &mode {
                    WriteMode::Shell if is_shell_c_flag(bare) => {
                        // `bash -c "<command line>"`: the next word starts a command.
                        mode = WriteMode::None;
                        command_position = true;
                    }
                    WriteMode::Interpreter(idx, inplace_capable)
                        if *inplace_capable && is_inplace_flag(bare) =>
                    {
                        scan.interpreters[*idx].1 = true;
                    }
                    WriteMode::OutputFlag(value_flags, bare_flags) => {
                        if value_flags.contains(&bare) {
                            if let Some(Tok::Word(next)) = toks.get(i + 1) {
                                scan.writes.push((unquote(next).to_string(), dir.clone()));
                                i += 1;
                            }
                        } else if bare_flags.contains(&bare) {
                            // `curl -O`: saves into the cwd.
                            scan.writes.push((".".to_string(), dir.clone()));
                        }
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
            match &mut mode {
                WriteMode::AllOperands => scan.writes.push((bare.to_string(), dir.clone())),
                WriteMode::LastOperand(operands) => operands.push(bare.to_string()),
                WriteMode::AfterFirst(control) => {
                    if control.is_none() {
                        // First operand is the control operand (mode / owner spec).
                        *control = Some(bare.to_string());
                    } else {
                        scan.writes.push((bare.to_string(), dir.clone()));
                    }
                }
                _ => {}
            }
            i += 1;
        }
        flush_last_operand(&mut mode, &dir, &mut scan);
        scan
    }
}

/// Working directory a command line segment runs in.
#[derive(Clone, Debug)]
enum Dir {
    Known(PathBuf),
    /// After a `cd` (or `popd`) whose target is not a literal.
    Unknown,
}

/// What [`SecurityGuardMiddleware::scan_command`] found.
#[derive(Default)]
struct Scan {
    /// Write-target strings, each paired with the directory it is evaluated in.
    writes: Vec<(String, Dir)>,
    /// Interpreter invocations: directory + whether an in-place flag
    /// (`perl -pi`) was seen.
    interpreters: Vec<(Dir, bool)>,
}

/// How operands of the current command are interpreted.
enum WriteMode {
    None,
    AllOperands,
    LastOperand(Vec<String>),
    AfterFirst(Option<String>),
    /// `curl` / `wget`: (flags whose next word is a target, bare flags that write to cwd).
    OutputFlag(&'static [&'static str], &'static [&'static str]),
    /// `bash` / `sh` / ...: a `-c` flag turns the next word into a command.
    Shell,
    /// An interpreter at `Scan::interpreters[idx]`; the bool = in-place flags apply.
    Interpreter(usize, bool),
}

fn flush_last_operand(mode: &mut WriteMode, dir: &Dir, scan: &mut Scan) {
    if let WriteMode::LastOperand(operands) = mode {
        if let Some(last) = operands.last() {
            scan.writes.push((last.clone(), dir.clone()));
        }
        operands.clear();
    }
}

/// Lexical unit of a command line.
enum Tok<'a> {
    Word(&'a str),
    /// A command boundary: newline, `;`, `|`, `||`, `&&`, `&`, `(`, `)`, backtick.
    Sep,
}

/// Byte ranges of heredoc bodies (including the terminating delimiter line).
/// Their content is data / another language's source, not shell.
///
/// A heredoc whose terminator never appears is NOT treated as a span: that
/// is far more likely a `<<` inside a quoted string, and swallowing the rest
/// of the command would let it skip the scan.
fn heredoc_spans(command: &str) -> Vec<std::ops::Range<usize>> {
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in command.split_inclusive('\n') {
        lines.push((offset, line));
        offset += line.len();
    }

    let mut spans = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx].1;
        idx += 1;

        let mut delims: Vec<String> = Vec::new();
        let mut search = line;
        while let Some(pos) = search.find("<<") {
            let rest = &search[pos + 2..];
            if let Some(after) = rest.strip_prefix('<') {
                search = after; // here-string `<<<`
                continue;
            }
            let rest = rest.strip_prefix('-').unwrap_or(rest).trim_start();
            let delim: String = rest
                .trim_start_matches(['"', '\'', '\\'])
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !delim.is_empty() {
                delims.push(delim);
            }
            search = rest;
        }

        for delim in delims {
            let Some(rel) = lines[idx..].iter().position(|(_, l)| l.trim() == delim) else {
                break;
            };
            let last = idx + rel;
            let body_start = lines[idx].0;
            let end = lines[last].0 + lines[last].1.len();
            spans.push(body_start..end);
            idx = last + 1;
        }
    }
    spans
}

/// Splits a command line into words and command boundaries. Heredoc bodies
/// are skipped; quotes are not tracked (a quoted `;` is also a boundary, which
/// only ever makes the scan stricter).
fn tokenize(command: &str) -> Vec<Tok<'_>> {
    let spans = heredoc_spans(command);
    let in_span = |pos: usize| spans.iter().any(|r| r.contains(&pos));
    let chars: Vec<(usize, char)> = command.char_indices().collect();
    let mut toks = Vec::new();
    let mut start: Option<usize> = None;
    fn flush<'a>(command: &'a str, toks: &mut Vec<Tok<'a>>, start: &mut Option<usize>, end: usize) {
        if let Some(s) = start.take() {
            toks.push(Tok::Word(&command[s..end]));
        }
    }
    let mut k = 0;
    while k < chars.len() {
        let (pos, c) = chars[k];
        let prev = if k > 0 { Some(chars[k - 1].1) } else { None };
        let next = chars.get(k + 1).map(|(_, c)| *c);
        if in_span(pos) {
            flush(command, &mut toks, &mut start, pos);
            k += 1;
            continue;
        }
        match c {
            '\\' if next == Some('\n') => {
                // line continuation
                flush(command, &mut toks, &mut start, pos);
                k += 2;
                continue;
            }
            '\n' | ';' | '|' | '(' | ')' | '`' => {
                flush(command, &mut toks, &mut start, pos);
                if !matches!(toks.last(), Some(Tok::Sep)) {
                    toks.push(Tok::Sep);
                }
            }
            '&' if next == Some('&') => {
                flush(command, &mut toks, &mut start, pos);
                if !matches!(toks.last(), Some(Tok::Sep)) {
                    toks.push(Tok::Sep);
                }
                k += 2;
                continue;
            }
            // Single `&` (background) is a boundary, but not in `2>&1` / `&>f`.
            '&' if prev != Some('>') && prev != Some('<') && next != Some('>') => {
                flush(command, &mut toks, &mut start, pos);
                if !matches!(toks.last(), Some(Tok::Sep)) {
                    toks.push(Tok::Sep);
                }
            }
            c if c.is_whitespace() => flush(command, &mut toks, &mut start, pos),
            _ => {
                if start.is_none() {
                    start = Some(pos);
                }
            }
        }
        k += 1;
    }
    flush(command, &mut toks, &mut start, command.len());
    toks
}

fn unquote(s: &str) -> &str {
    s.trim_matches(|c| c == '"' || c == '\'')
}

fn basename(verb: &str) -> &str {
    verb.rsplit('/').next().unwrap_or(verb)
}

/// `NAME=value` shell assignment prefix.
fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

fn is_interpreter(verb: &str) -> bool {
    verb.starts_with("python") || INTERPRETERS.contains(&verb)
}

/// `-c`, `-lc`, `-ic`, `-ec`, ...: a shell flag cluster ending in `c`.
fn is_shell_c_flag(flag: &str) -> bool {
    flag.len() <= 4
        && flag.starts_with('-')
        && !flag.starts_with("--")
        && flag.ends_with('c')
        && flag[1..].chars().all(|c| c.is_ascii_alphabetic())
}

/// `perl -i`, `-pi`, `-pie`, `-i.bak`: in-place edit clusters.
fn is_inplace_flag(flag: &str) -> bool {
    let body = flag.strip_prefix('-').unwrap_or(flag);
    if body.starts_with('-') || body.is_empty() {
        return false;
    }
    let letters: String = body
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    letters.contains('i') && letters.chars().all(|c| c.is_ascii_lowercase())
}

/// Script text with escaping removed and stdout/stderr writes (which never
/// touch files) blanked out, so they do not look like file writes.
fn clean_script_text(command: &str) -> String {
    let mut text = command.replace('\\', "");
    for benign in [
        "sys.stdout.write",
        "sys.stderr.write",
        "process.stdout.write",
        "process.stderr.write",
    ] {
        text = text.replace(benign, "");
    }
    text
}

/// Whether script source contains a file-writing / process-spawning construct.
fn script_writes(text: &str) -> bool {
    if SCRIPT_WRITE_MARKERS.iter().any(|m| text.contains(m)) {
        return true;
    }
    // `open(p, "w")` and friends: `open(` alone is just as often a read.
    text.contains("open(") && SCRIPT_WRITE_MODES.iter().any(|m| text.contains(m))
}

/// Absolute / home-relative / `..` path literals appearing in script source.
fn script_path_literals(text: &str) -> Vec<String> {
    text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '(' | ')' | ',' | ';' | '=' | '[' | ']' | '{' | '}' | '`' | '<' | '>'
            )
    })
    .filter(|lit| {
        (lit.starts_with('/') && lit.len() > 1 && !lit.starts_with("//"))
            || lit.starts_with("~/")
            || lit.starts_with("../")
            || lit.contains("/../")
            || *lit == ".."
    })
    .map(str::to_string)
    .collect()
}

/// Extracts candidate write-target strings from a shell command line (the
/// directory-resolution layer is dropped; see `scan_command` for the full
/// picture). Kept as a pure helper for tests.
#[cfg(test)]
fn extract_write_targets(command: &str) -> Vec<String> {
    let guard = SecurityGuardMiddleware::new(std::env::temp_dir());
    guard
        .scan_command(command, Dir::Known(std::env::temp_dir()))
        .writes
        .into_iter()
        .map(|(t, _)| t)
        .collect()
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

/// Resolves a path to its true filesystem location, walking it component by
/// component the way the kernel would:
///
///   * every existing prefix is canonicalized (symlinks — including `/var →
///     /private/var` — expanded), so `link/..` follows the link's real parent;
///   * a segment that does not exist cannot be a symlink, so it and any `..`
///     after it are applied lexically (`ws/missing/../../x` is `parent(ws)/x`).
///
/// Expects an absolute path; a relative one is resolved relative to nothing.
fn resolve_for_check(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => {
                out.push(name);
                if let Ok(canonical) = out.canonicalize() {
                    out = canonical;
                }
            }
        }
    }
    out
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

        // 1. Inspect arguments for path traversal across standard target fields
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.function.arguments) {
            let path_str = args
                .get("path")
                .or_else(|| args.get("file_path"))
                .or_else(|| args.get("filePath"))
                .or_else(|| args.get("target"))
                .and_then(|v| v.as_str());

            if let Some(path_str) = path_str {
                if let Err(violation) = self.check_path(Path::new(path_str)) {
                    let notice = SystemNotice::new(
                        "SecurityGuard",
                        "Path traversal attack blocked",
                        "No file system modifications were performed. Command was halted at security perimeter.",
                    )
                    .with_guidance(format!(
                        "Confine all file operations within the allowed workspace roots: [{}]. \
                         Paths inside any listed root are read/write accessible. \
                         Do not try to reach outside paths through bash, interpreters or other tools; \
                         if the task genuinely needs another directory, ask the user to grant it \
                         (TUI: `/roots add <path>`; or restart from that directory / with `--workspace <path>`).",
                        self.roots_display()
                    ));

                    return ToolExecutionResult::error(
                        format!("Access Denied: {}", violation),
                        start.elapsed(),
                    )
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
                        "Ensure working directory targets remain inside the allowed roots: [{}]. \
                         To work elsewhere, ask the user to run `/roots add <path>`.",
                        self.roots_display()
                    ));

                    return ToolExecutionResult::error(
                        format!("Access Denied: {}", violation),
                        start.elapsed(),
                    )
                    .with_telemetry(notice);
                }
            }

            // Check bash commands for high-risk patterns and out-of-jail writes
            if call.function.name == "bash" {
                if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                    // The command starts in the call's `cwd` when given (already
                    // validated above), else the primary workspace root.
                    let start_dir = args
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .and_then(|c| self.check_path(Path::new(c)).ok())
                        .unwrap_or_else(|| self.workspace_root.clone());
                    if let Err(violation) = self.check_command_in(cmd, &start_dir) {
                        let is_write_escape = violation.starts_with("Shell write target");
                        let (action, ground_truth, guidance): (&str, &str, String) =
                            if is_write_escape {
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

                        return ToolExecutionResult::error(
                            format!("Execution Blocked: {}", violation),
                            start.elapsed(),
                        )
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
            ..Default::default()
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

    async fn run(
        guard: &SecurityGuardMiddleware,
        call: &ToolCall,
        id: &str,
    ) -> ToolExecutionResult {
        guard
            .handle(call, &ctx(id), None, Arc::new(DummyNext))
            .await
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
        assert!(res
            .output
            .contains(ws.canonicalize().unwrap().to_string_lossy().as_ref()));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_forbidden_command_blocked() {
        let ws = std::env::temp_dir().join("thunder_sec_test_cmd");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        let res = run(
            &guard,
            &bash_call("call_sec_2", "sudo rm -rf /"),
            "call_sec_2",
        )
        .await;
        assert!(res.is_error);
        assert!(res.output.contains("Forbidden high-risk command pattern"));
        assert!(res
            .output
            .contains("Dangerous destructive shell operations are disabled"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_valid_path_allowed() {
        let ws = std::env::temp_dir().join("thunder_sec_test_ok");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        let res = run(
            &guard,
            &write_call("call_sec_3", "src/main.rs"),
            "call_sec_3",
        )
        .await;
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
        let res = run(
            &guard,
            &write_call("call_m1", target.to_str().unwrap()),
            "call_m1",
        )
        .await;
        assert!(!res.is_error, "extra root path must pass");

        // Relative path still resolves against the primary root.
        let res = run(&guard, &write_call("call_m2", "notes.txt"), "call_m2").await;
        assert!(!res.is_error);

        // Outside all roots still blocked, and both roots are named.
        let res = run(&guard, &write_call("call_m3", "/etc/shadow"), "call_m3").await;
        assert!(res.is_error);
        assert!(res.output.contains("escapes all allowed workspace roots"));
        assert!(res
            .output
            .contains(repo.canonicalize().unwrap().to_string_lossy().as_ref()));

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
            "echo hacked > ../escaped.txt",
            "echo hacked > ~/.bashrc",
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
        assert!(
            targets.is_empty(),
            "echo operands must not be targets: {targets:?}"
        );

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

    // ── Bypass hardening: cd tracking, interpreters, substitutions ──────────

    fn bypass_dirs(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let ws = std::env::temp_dir().join(format!("thunder_sec_{name}_ws"));
        let outside = std::env::temp_dir().join(format!("thunder_sec_{name}_outside"));
        let _ = std::fs::create_dir_all(ws.join("src"));
        let _ = std::fs::create_dir_all(&outside);
        (ws, outside)
    }

    async fn blocked(guard: &SecurityGuardMiddleware, cmd: &str) {
        let res = run(guard, &bash_call("call_bp", cmd), "call_bp").await;
        assert!(res.is_error, "must block: {cmd:?}");
        assert!(
            res.output.contains("Shell write target") || res.output.contains("Forbidden"),
            "wrong reason for {cmd:?}: {}",
            res.output
        );
    }

    async fn allowed(guard: &SecurityGuardMiddleware, cmd: &str) {
        let res = run(guard, &bash_call("call_bp", cmd), "call_bp").await;
        assert!(!res.is_error, "must allow: {cmd:?} ({})", res.output);
    }

    /// The exact shape that escaped the jail in a real session:
    /// `cd <outside> && python3 - <<'PY' ... io.open(p, "w") ... PY`.
    #[tokio::test]
    async fn test_cd_outside_then_interpreter_write_blocked() {
        let (ws, outside) = bypass_dirs("cdpy");
        let guard = SecurityGuardMiddleware::new(&ws);
        let o = outside.display();

        blocked(
            &guard,
            &format!(
                "cd {o} && python3 - <<'PY'\nimport io\np=\"doc.md\"\nio.open(p,\"w\",encoding=\"utf-8\").write(\"x\")\nPY\nsed -n '1p' doc.md"
            ),
        )
        .await;
        blocked(
            &guard,
            &format!("cd {o} && python3 -c \"open('f.txt','w').write('x')\""),
        )
        .await;
        blocked(
            &guard,
            &format!("cd {o} && node -e \"require('fs').writeFileSync('f.txt','x')\""),
        )
        .await;
        blocked(&guard, &format!("cd {o} && perl -pi -e 's/a/b/' f.txt")).await;

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn test_cd_outside_then_shell_write_blocked() {
        let (ws, outside) = bypass_dirs("cdsh");
        let guard = SecurityGuardMiddleware::new(&ws);
        let o = outside.display();

        for cmd in [
            format!("cd {o} && touch x"),
            format!("cd {o}; rm x"),
            format!("cd {o} && echo hi > x.txt"),
            format!("cd {o}\nrm x"),
            format!("pushd {o} && cp a b"),
            format!("cd '{o}' && mkdir d"),
            "cd .. && touch x".to_string(),
            "cd ../.. && touch x".to_string(),
            "touch a/../../x".to_string(),
            "cd src && cd ../.. && touch x".to_string(),
            format!("bash -c \"cd {o} && touch x\""),
            format!("sh -c 'cd {o}; echo hi > x'"),
            // unknown cwd after a non-literal cd: relative writes cannot be judged
            "cd \"$SOMEWHERE\" && touch x".to_string(),
            "cd - && touch x".to_string(),
        ] {
            blocked(&guard, &cmd).await;
        }

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn test_interpreter_absolute_path_writes_blocked() {
        let (ws, outside) = bypass_dirs("interp");
        let guard = SecurityGuardMiddleware::new(&ws);

        for cmd in [
            "python3 -c \"open('/etc/hosts','w').write('x')\"",
            "python -c \"import os; os.remove('/etc/hosts')\"",
            "node -e \"require('fs').writeFileSync('/etc/hosts','x')\"",
            "ruby -e 'File.write(\"/etc/hosts\", \"x\")'",
            "perl -pi -e 's/a/b/' /etc/hosts",
            "python3 -c \"import shutil; shutil.copy('a', '../escape')\"",
            "python3 - <<'PY'\nfrom pathlib import Path\nPath('~/x').expanduser()\nPath('/etc/hosts').write_text('x')\nPY",
        ] {
            blocked(&guard, cmd).await;
        }

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn test_wrappers_substitutions_and_other_writers_blocked() {
        let (ws, outside) = bypass_dirs("wrap");
        let guard = SecurityGuardMiddleware::new(&ws);

        for cmd in [
            "echo hi\nrm /etc/hosts",
            "echo hi;rm /etc/hosts",
            "FOO=1 rm /etc/hosts",
            "env FOO=1 rm /etc/hosts",
            "sudo rm /etc/hosts",
            "/bin/rm /etc/hosts",
            "command rm /etc/hosts",
            "echo $(rm /etc/hosts)",
            "echo `rm /etc/hosts`",
            "(rm /etc/hosts)",
            "true && { rm /etc/hosts; }",
            "if true; then rm /etc/hosts; fi",
            "for f in a; do rm /etc/hosts; done",
            "eval \"rm /etc/hosts\"",
            "dd of=/etc/hosts",
            "curl -o /etc/hosts http://example.com",
            "wget -O /etc/hosts http://example.com",
            "rsync -a src/ /etc/hosts",
            // target starts with a substitution: location unknowable
            "echo x > $OUT",
            "echo x > \"$OUT/file\"",
            "rm \"$f\"",
            "echo x > `mktemp`",
            "echo x > $HOME/.bashrc",
        ] {
            blocked(&guard, cmd).await;
        }

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The hardening must not turn into a wall: ordinary in-jail work, reads
    /// outside the jail, and read-only interpreter use keep working.
    #[tokio::test]
    async fn test_hardening_keeps_legitimate_commands_working() {
        let (ws, outside) = bypass_dirs("legit");
        let guard = SecurityGuardMiddleware::new(&ws);
        let o = outside.display();
        let w = ws.display();

        for cmd in [
            "cd src && touch x.txt".to_string(),
            "cd src && cd .. && touch ok.txt".to_string(),
            format!("cd {w}/src && echo hi > a.txt"),
            // reading outside the jail through bash is not a write
            format!("cd {o} && ls && cat notes.md | head -5"),
            format!("cd {o} && python3 -c \"print(open('f').read())\""),
            format!("cd {o} && sed -n '1,5p' file.txt"),
            "python3 - <<'PY'\nprint('hi')\nPY".to_string(),
            "python3 -c \"import sys; sys.stdout.write('x')\"".to_string(),
            "python3 -c \"open('out.txt','w').write('x')\"".to_string(),
            "node -e \"require('fs').writeFileSync('out.txt','x')\"".to_string(),
            "python3 manage.py migrate".to_string(),
            "for f in a b; do echo $f > out/$f.txt; done".to_string(),
            "echo done 2>&1 | tee out.log".to_string(),
            "echo cd /somewhere; touch y".to_string(),
            "sed 's/a/b/' file.txt".to_string(),
            "curl -s http://example.com | head".to_string(),
            "ls -la 2>&1".to_string(),
            "echo a && echo b || echo c".to_string(),
            "cat <<'EOF' > notes.md\nrm /etc/hosts\necho x > /etc/hosts\nEOF".to_string(),
            "git status && git diff > changes.patch".to_string(),
        ] {
            allowed(&guard, &cmd).await;
        }

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// Commands after a heredoc are still shell: the body is skipped, not the rest.
    #[tokio::test]
    async fn test_heredoc_body_skipped_but_following_commands_scanned() {
        let (ws, outside) = bypass_dirs("heredoc");
        let guard = SecurityGuardMiddleware::new(&ws);

        blocked(&guard, "cat <<'EOF' > notes.md\nhello\nEOF\nrm /etc/hosts").await;
        // An unterminated `<<` (e.g. inside a quoted string) must not swallow the rest.
        blocked(&guard, "python3 -c \"print(1<<2)\"\nrm /etc/hosts").await;

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// A tool call's explicit `cwd` is the starting directory for relative targets.
    #[tokio::test]
    async fn test_bash_cwd_argument_is_the_start_directory() {
        let (ws, outside) = bypass_dirs("cwdarg");
        let repo = std::env::temp_dir().join("thunder_sec_cwdarg_repo");
        let _ = std::fs::create_dir_all(&repo);
        let guard = SecurityGuardMiddleware::new(&ws).with_extra_roots([&repo]);

        let call = ToolCall::new_function(
            "call_cwd",
            "bash",
            serde_json::json!({ "command": "touch x.txt", "cwd": repo.to_string_lossy() })
                .to_string(),
        );
        let res = run(&guard, &call, "call_cwd").await;
        assert!(
            !res.is_error,
            "cwd in an extra root is a valid start: {}",
            res.output
        );

        // `..` out of that cwd leaves every root.
        let call = ToolCall::new_function(
            "call_cwd2",
            "bash",
            serde_json::json!({ "command": "touch ../x.txt", "cwd": repo.to_string_lossy() })
                .to_string(),
        );
        let res = run(&guard, &call, "call_cwd2").await;
        assert!(res.is_error);

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// `..` after a path segment that does not exist yet must still climb out:
    /// the old resolver dropped it and let `ws/missing/../../x` through.
    #[tokio::test]
    async fn test_write_file_dotdot_through_missing_dir_blocked() {
        let (ws, outside) = bypass_dirs("dotdot");
        let guard = SecurityGuardMiddleware::new(&ws);

        for path in [
            "missing/../../escaped.txt",
            "src/../../escaped.txt",
            "a/b/../../../x",
        ] {
            let res = run(&guard, &write_call("call_dd", path), "call_dd").await;
            assert!(res.is_error, "must block write_file to {path:?}");
            assert!(res.output.contains("escapes all allowed workspace roots"));
        }
        // Still fine when the `..` stays inside the jail.
        let res = run(
            &guard,
            &write_call("call_dd2", "missing/../ok.txt"),
            "call_dd2",
        )
        .await;
        assert!(!res.is_error, "{}", res.output);

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
