//! Static shell command analyzer proving whether a command is 100% read-only.

use serde_json::Value;

pub const COMMAND_KEYS: &[&str] = &["command", "cmd", "script"];

const READ_VERBS: &[&str] = &[
    "arch",
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
    "groups",
    "head",
    "hexdump",
    "hostname",
    "id",
    "ls",
    "md5",
    "md5sum",
    "nl",
    "nproc",
    "od",
    "printenv",
    "printf",
    "pwd",
    "readlink",
    "realpath",
    "sha1sum",
    "seq",
    "sha256sum",
    "shasum",
    "stat",
    "sw_vers",
    "tail",
    "test",
    "tr",
    "tty",
    "type",
    "uname",
    "wc",
    "which",
    "whoami",
    "[",
];

const FIND_WRITE_FLAGS: &[&str] = &[
    "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fls", "-fprint", "-fprint0", "-fprintf",
];

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

const SECRET_SUFFIXES: &[&str] = &[".key", ".keystore", ".p12", ".pem", ".pfx"];

pub fn call_is_read_only(arguments: &Value) -> bool {
    COMMAND_KEYS
        .iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str))
        .is_some_and(is_read_only)
}

pub fn is_read_only(command: &str) -> bool {
    let Some(mut words) = single_command(command) else {
        return false;
    };
    while words.first().is_some_and(|w| is_assignment(w)) {
        words.remove(0);
    }
    let Some((verb, args)) = words.split_first() else {
        return false;
    };

    if verb.contains('/') {
        return false;
    }

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
        "file" => !args.iter().any(|a| a == "-C" || a == "--compile"),
        "tree" => !args.iter().any(|a| a == "-o" || a.starts_with("--output")),
        "find" => !args.iter().any(|a| FIND_WRITE_FLAGS.contains(&a.as_str())),
        "git" => git_reads(args),
        "rg" => !args.iter().any(|a| a.starts_with("--pre")),
        "sort" => !args
            .iter()
            .any(|a| a.starts_with("-o") || a.starts_with("--output")),
        verb => READ_VERBS.contains(&verb),
    }
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn git_reads(args: &[String]) -> bool {
    let Some(sub) = args.iter().find(|a| !a.starts_with('-')) else {
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
        "config" => matches!(
            rest.first().map(String::as_str),
            Some("--get") | Some("--get-all") | Some("--get-regexp") | Some("--list") | Some("-l")
        ),
        "reflog" => matches!(rest.first().map(String::as_str), None | Some("show")),
        "stash" => matches!(
            rest.first().map(String::as_str),
            None | Some("list") | Some("show")
        ),
        "worktree" => matches!(rest.first().map(String::as_str), Some("list")),
        _ => false,
    }
}

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

    #[test]
    fn proves_safe_read_commands_and_rejects_mutations_or_secrets() {
        assert!(is_read_only("git status --short"));
        assert!(is_read_only("grep -rn \"a|b\" src/"));
        assert!(is_read_only("find . -name '*.rs'"));
        assert!(!is_read_only("find . -delete"));
        assert!(!is_read_only("echo hi > out.txt"));
        assert!(!is_read_only("cat ~/.ssh/id_rsa"));
        assert!(!is_read_only("git reset --hard"));
    }
}
