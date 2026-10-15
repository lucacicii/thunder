//! The shell commands plan mode may run without asking.
//!
//! Plan mode's rule is "nothing here changes anything", and a shell command is
//! the one call whose effect cannot be read off its name: `ls` and `rm -rf /`
//! arrive as the same tool with the same declared effect. So the question this
//! file answers is deliberately pessimistic: *is this command line, as written,
//! incapable of changing anything?* What it cannot prove is left to the gate as
//! a question — the same bargain [`guard.rs`](super::guard) makes in the other
//! direction, where an unparseable command is refused nothing and asks instead.
//!
//! What counts as proof:
//!
//! * **One command.** A separator or a redirect — `;`, `&`, `|`, `` ` ``, `>`,
//!   `<`, `(`, `)`, `{`, `}`, a newline — is a second command nobody looked at,
//!   or a file being written. An unquoted one is refused outright; a `|` inside
//!   quotes is an argument, so `grep "a|b" src/` stays a read. `$` is refused
//!   because a substitution is a command that cannot be seen at all.
//! * **A verb that only reads**, from a list short enough to read in one
//!   sitting. The few verbs that *can* write through a flag are not on it; they
//!   get their own arm (`sort -o`, `find -delete`, `git branch -D`), because
//!   dropping them entirely would ask about the most common inspections there
//!   are.
//! * **A verb, not a path.** `/tmp/evil/ls` is not `ls`; only a bare word is
//!   matched, so a workspace that ships its own `ls` does not become a way to
//!   run it unattended. The cost is that `/bin/ls` asks, which is the direction
//!   a false negative is allowed to fail in.
//! * **No wrapper by omission.** `sudo`, `env`, `xargs` run *another* verb, so
//!   the verb that was checked is not the one that runs. They are not on the
//!   list, and nothing here has to know their names.
//! * **No operand that names a secret.** The allowance is about *reading* being
//!   harmless, and a credential file is the one read that is not — `cat
//!   ~/.ssh/id_rsa` would put a private key in the transcript. The list of
//!   components is short and matches whole path components, never substrings,
//!   and it only ever turns an allowance into a question.
//!
//! The honest limit: this reads a command line, it is not a sandbox. A program
//! that reads something surprising but is not on the secret list is allowed, and
//! a command assembled at runtime cannot be recognised at all — which is why
//! this is consulted in plan mode *only*, and only to choose between running
//! silently and asking.

/// Verbs that only read, whatever they are given.
///
/// Every entry earns its place: it must be incapable of writing a file, running
/// another program, or reaching the network. That excludes more than it looks
/// like it should — `jq` grew a `system()` builtin, `rg --pre` runs a command per
/// file, `sed -i` and `uniq in out` write, `less` waits for a keystroke — and the
/// ones with a single dangerous flag got an arm of their own below instead.
const READ_VERBS: &[&str] = &[
    "basename",
    "cat",
    "cksum",
    "column",
    "comm",
    "cut",
    "df",
    "diff",
    "dirname",
    "du",
    "echo",
    "grep",
    "head",
    "hexdump",
    "ls",
    "md5",
    "md5sum",
    "nl",
    "od",
    "printf",
    "pwd",
    "readlink",
    "realpath",
    "sha1sum",
    "sha256sum",
    "shasum",
    "stat",
    "tail",
    "tr",
    "type",
    "wc",
    "which",
];

/// `find` flags that run something or write a file, rather than print a name.
const FIND_WRITE_FLAGS: &[&str] = &[
    "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fls", "-fprint", "-fprint0", "-fprintf",
];

/// `git` subcommands that only read, whatever they are given.
const GIT_READ_SUBCOMMANDS: &[&str] = &[
    "blame",
    "cat-file",
    "check-ignore",
    "count-objects",
    "describe",
    "diff",
    "diff-tree",
    "for-each-ref",
    "grep",
    "log",
    "ls-files",
    "ls-tree",
    "merge-base",
    "name-rev",
    "rev-parse",
    "shortlog",
    "show",
    "show-ref",
    "status",
    "version",
    "whatchanged",
];

/// Flags that leave `git branch` and `git tag` listing rather than changing.
///
/// A bare `git branch` lists, `git branch -a` lists, and `git branch foo`
/// *creates* — which is why the arm below requires every argument to be one of
/// these. A flag that takes a value (`--contains <rev>`) drags a bare word in,
/// and asks; listing flags that pair with a value are not worth the parse.
const GIT_LIST_FLAGS: &[&str] = &[
    "-a",
    "-i",
    "-l",
    "-q",
    "-r",
    "-v",
    "-vv",
    "--all",
    "--column",
    "--ignore-case",
    "--list",
    "--merged",
    "--no-column",
    "--no-merged",
    "--points-at",
    "--quiet",
    "--remotes",
    "--verbose",
];

/// Path components and suffixes that name a credential.
///
/// Matched as whole components (`components().any(...)`), never as substrings,
/// so `docs/ssh-notes.md` and `test/.environment` stay ordinary reads.
///
/// Every entry is a name that is a key and nothing else — a dotted dotfile or an
/// `id_*` key. The bare word `credentials` is deliberately absent: it is also a
/// word people grep for, and the `credentials.json` that thunder's host
/// classifies as critical is refused by the guard long before this runs.
const SECRET_COMPONENTS: &[&str] = &[
    ".aws",
    ".docker",
    ".env",
    ".git-credentials",
    ".gnupg",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".ssh",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_rsa",
];

/// Suffixes that name a key or a keystore rather than a document.
const SECRET_SUFFIXES: &[&str] = &[".key", ".keystore", ".p12", ".pem", ".pfx"];

/// Whether an exec call is a read, from the arguments the call would run.
///
/// The entry point for a gate: it takes the arguments rather than the command
/// because the arguments are what a gate has, and it looks in the same keys the
/// guard does so a tool that spells its command `cmd` or `script` is judged like
/// one that spells it `command`. A call with no readable command is not a read —
/// the same fail-closed answer as a command this cannot parse.
pub fn call_is_read_only(arguments: &serde_json::Value) -> bool {
    crate::tools::guard::COMMAND_KEYS
        .iter()
        .find_map(|key| arguments.get(*key).and_then(serde_json::Value::as_str))
        .is_some_and(is_read_only)
}

/// Whether `command` can be shown to change nothing.
///
/// `true` means plan mode runs it without a dialog. `false` means the gate asks,
/// which is the answer for everything this cannot read.
pub fn is_read_only(command: &str) -> bool {
    let Some(words) = single_command(command) else {
        return false;
    };
    let Some((verb, args)) = words.split_first() else {
        return false;
    };

    // A bare verb, never a path to one: `/tmp/evil/ls` would otherwise be
    // judged by its basename while running whatever it is.
    if verb.contains('/') {
        return false;
    }

    // Reading a credential is the one read that is not harmless, so an operand
    // that names one turns the allowance back into a question. Flags are checked
    // too: `--file=~/.ssh/id_rsa` is the same read in another spelling.
    if args.iter().any(|a| {
        let path = std::path::Path::new(a);
        path.components()
            .any(|c| SECRET_COMPONENTS.contains(&c.as_os_str().to_string_lossy().as_ref()))
            || SECRET_SUFFIXES
                .iter()
                .any(|s| a.to_ascii_lowercase().ends_with(s))
    }) {
        return false;
    }

    match verb.as_str() {
        // A magic-file compile writes the compiled result out.
        "file" => !args.iter().any(|a| a == "-C" || a == "--compile"),
        "find" => !args.iter().any(|a| FIND_WRITE_FLAGS.contains(&a.as_str())),
        "git" => git_reads(args),
        // `--pre` runs a command for every file it matches.
        "rg" => !args.iter().any(|a| a.starts_with("--pre")),
        // `-o FILE` is the one way `sort` writes, in three spellings.
        "sort" => !args
            .iter()
            .any(|a| a.starts_with("-o") || a.starts_with("--output")),
        verb => READ_VERBS.contains(&verb),
    }
}

/// Whether a `git` invocation only reads.
///
/// The subcommand is the first word that is not a flag, which keeps this
/// ignorant of `git`'s global options: `git -C /ws status` reads its subcommand
/// as `/ws` and asks. Asking is the correct answer for a form this does not
/// parse, and the common spellings — `git status`, `git log -p` — never reach
/// that path.
fn git_reads(args: &[String]) -> bool {
    let Some(sub) = args.iter().find(|a| !a.starts_with('-')) else {
        // No subcommand at all: only the flags that report something.
        return args.iter().all(|a| {
            matches!(
                a.as_str(),
                "--version" | "-v" | "--help" | "--no-pager" | "--paginate" | "--no-optional-locks"
            )
        });
    };
    let rest = &args[args.iter().position(|a| a == sub).unwrap_or(0) + 1..];

    if GIT_READ_SUBCOMMANDS.contains(&sub.as_str()) {
        return true;
    }

    match sub.as_str() {
        // Listing forms read; every other form of these moves a ref.
        "branch" | "tag" => {
            rest.is_empty()
                || rest.iter().all(|a| {
                    GIT_LIST_FLAGS.contains(&a.as_str())
                        || a.starts_with("--format=")
                        || a.starts_with("--sort=")
                })
        }
        "remote" => rest
            .iter()
            .all(|a| matches!(a.as_str(), "-v" | "--verbose")),
        "reflog" => matches!(rest.first().map(String::as_str), None | Some("show")),
        "stash" => matches!(
            rest.first().map(String::as_str),
            None | Some("list") | Some("show")
        ),
        "worktree" => matches!(rest.first().map(String::as_str), Some("list")),
        _ => false,
    }
}

/// The words of one command line, or `None` when the line is not one command.
///
/// Quoting is tracked, which is the whole reason this exists: `grep -rn "a|b"
/// src/` is a read, and a `|` inside quotes is an argument rather than a pipe.
/// Everything the shell itself would act on is refused unquoted — including a
/// backslash, which can hide a separator (`ls \; rm -rf /` is not a read
/// however it is parsed) — and an unterminated quote is not a command this can
/// read at all.
fn single_command(command: &str) -> Option<Vec<String>> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();

    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    word.push(chars.next()?);
                } else {
                    word.push(c);
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '\\' => return None,
                ';' | '\n' | '|' | '&' | '`' | '(' | ')' | '{' | '}' | '>' | '<' | '$' => {
                    return None
                }
                c if c.is_whitespace() => {
                    if !word.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                }
                _ => word.push(c),
            },
        }
    }

    if quote.is_some() {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }
    (!words.is_empty()).then_some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(command: &str) -> bool {
        is_read_only(command)
    }

    /// The inspections plan mode exists to run without a question.
    #[test]
    fn reads_run_unattended() {
        for command in [
            "ls",
            "ls -la",
            "ls -la src/",
            "cat README.md",
            "cat /var/folders/jg/xx/T/clipboard.png",
            "file '/var/folders/jg/xx/T/agent-resume-clipboard-3f1a.png'",
            "head -n 20 Cargo.toml",
            "tail -f logs/tui.log",
            "wc -l src/lib.rs",
            "stat -f %z image.png",
            "grep -rn foo src/",
            "grep -rn \"a|b\" src/",
            "rg --hidden TODO",
            "find . -name '*.rs'",
            "find src -type f -newer Cargo.toml",
            "sort src/lib.rs",
            "sort -n numbers.txt",
            "git status",
            "git status --short",
            "git log -p --stat",
            "git diff HEAD~1",
            "git show HEAD:README.md",
            "git branch",
            "git branch -a",
            "git tag -l",
            "git stash list",
            "git worktree list",
            "git remote -v",
            "git rev-parse --show-toplevel",
            "diff a.txt b.txt",
            "dirname src/main.rs",
            "realpath ./src",
            "which cargo",
            "echo hello",
        ] {
            assert!(allowed(command), "{command} should run in plan mode");
        }
    }

    /// Everything that changes something, or could: this is the list that keeps
    /// the allowance from being a way to write while planning.
    #[test]
    fn anything_that_could_change_something_asks_instead() {
        for command in [
            // A second command nobody checked.
            "ls; rm -rf /",
            "ls && rm -rf build",
            "ls || true",
            "cat a | tee b",
            "echo hi > out.txt",
            "cat < in.txt",
            "ls `pwd`",
            "ls $(pwd)",
            "ls &",
            "cat a.txt\nrm -rf build",
            // A separator hidden behind an escape.
            "ls \\; rm -rf /",
            // A verb that writes, or runs something.
            "rm -rf build",
            "mv a b",
            "cp a b",
            "touch new",
            "mkdir dir",
            "sed -i 's/a/b/' f",
            "awk '{print}' f",
            "python3 -c 'import shutil'",
            "node -e 'process.exit()'",
            "sudo ls",
            "env ls",
            "xargs rm",
            "tee out.txt",
            "sh -c ls",
            // A reader with a flag that changes it into a writer.
            "sort -o sorted.txt numbers.txt",
            "sort --output=sorted.txt numbers.txt",
            "find . -delete",
            "find . -exec rm {} ;",
            "rg --pre 'cat' pattern",
            "file -C -m magic",
            // `git` subcommands and forms that move a ref.
            "git push",
            "git commit -m x",
            "git checkout main",
            "git reset --hard",
            "git branch new-branch",
            "git branch -D old",
            "git tag v1.0",
            "git tag -a v1 -m x",
            "git remote add origin url",
            "git stash push",
            "git stash drop",
            "git worktree add ../wt",
            "git reflog expire --all",
            "git config user.email x",
            "git clean -fd",
            // A verb reached by a path, or a shape that is not one command.
            "/bin/ls",
            "./ls",
            "ls \"unterminated",
            "",
            "   ",
        ] {
            assert!(!allowed(command), "{command} must not run unattended");
        }
    }

    /// The one read that is not harmless: the allowance asks rather than putting
    /// a key in the transcript.
    #[test]
    fn reading_a_credential_asks_instead() {
        for command in [
            "cat ~/.ssh/id_rsa",
            "cat /Users/u/.aws/credentials",
            "cat .env",
            "cat server.pem",
            "cat cert.p12",
            "grep -r token ~/.npmrc",
        ] {
            assert!(!allowed(command), "{command} must ask");
        }

        // Whole components only: a document that mentions one is a read.
        for command in [
            "cat docs/ssh-notes.md",
            "grep -rn credentials src/",
            "cat test/.environment",
            "ls -la .ssh-keys-backup",
        ] {
            assert!(allowed(command), "{command} is an ordinary read");
        }
    }
}
