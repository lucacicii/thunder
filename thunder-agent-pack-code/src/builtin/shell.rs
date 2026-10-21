//! Which shell runs `bash`'s command lines.
//!
//! The tool is called `bash` because that is what the model learned to call;
//! the *program* is a host decision. A machine may have `zsh` as the login
//! shell and no `bash` at all, or a pinned `/bin/sh` for a reproducible run.
//!
//! Only shells that accept `-c <string>` and the POSIX-ish syntax the tool's
//! guidance assumes are eligible. A shell that does not understand the command
//! line fails loudly (`127` / syntax error) instead of silently doing something
//! else, so detection stays conservative.

use std::path::{Path, PathBuf};

/// The program and flag used to run one command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfig {
    program: PathBuf,
    /// The flag that makes the program read its script from the next argument.
    flag: String,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self::detect()
    }
}

impl ShellConfig {
    /// Build from an explicit program path.
    ///
    /// `-c` is the one flag every shell in [`SHELLS`] accepts; it is spelled
    /// here once so a host cannot hand in a program with different semantics.
    pub fn from_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            flag: "-c".to_string(),
        }
    }

    /// The user's shell when it is one this tool can drive, else `/bin/sh`.
    ///
    /// `$SHELL` is the login shell — the one the user's own terminal runs —
    /// so preferring it keeps `PATH` additions and quoting behaviour familiar.
    /// It is only a *preference*: a value that is not one of [`SHELLS`], or a
    /// path that no longer exists, falls back to `/bin/sh`.
    pub fn detect() -> Self {
        if let Some(shell) = std::env::var_os("SHELL") {
            let path = PathBuf::from(shell);
            if is_supported(&path) && path.is_file() {
                return Self::from_program(path);
            }
        }
        let sh = PathBuf::from("/bin/sh");
        if sh.is_file() {
            return Self::from_program(sh);
        }
        // Last resort: leave it to `PATH`. A machine without `/bin/sh` is not
        // POSIX, and failing at spawn time is clearer than guessing again.
        Self::from_program("sh")
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn flag(&self) -> &str {
        &self.flag
    }
}

/// Shell names whose `-c` behaviour this tool relies on.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "ash"];

fn is_supported(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| SHELLS.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_always_returns_a_program() {
        let shell = ShellConfig::detect();
        assert!(!shell.program().as_os_str().is_empty());
        assert_eq!(shell.flag(), "-c");
    }

    #[test]
    fn an_explicit_program_is_used_verbatim() {
        let shell = ShellConfig::from_program("/bin/sh");
        assert_eq!(shell.program(), Path::new("/bin/sh"));
    }

    #[test]
    fn only_shells_with_the_same_dash_c_contract_are_recognised() {
        assert!(is_supported(Path::new("/bin/bash")));
        assert!(is_supported(Path::new("/usr/local/bin/zsh")));
        assert!(is_supported(Path::new("/bin/sh")));
        assert!(!is_supported(Path::new("/bin/fish")));
        assert!(!is_supported(Path::new("/bin/csh")));
        assert!(!is_supported(Path::new("/bin/ls")));
    }
}
