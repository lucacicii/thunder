//! Putting text on the system clipboard.
//!
//! Same shape as [`crate::links`]: no new dependency, just the platform's own
//! tool, spawned with the text on its stdin. The candidate list is a pure
//! function so tests can pin it down without touching a real clipboard.

use std::io::{self, Write};
use std::process::{Command, Stdio};

/// Clipboard tools to try, best first. A missing tool moves on to the next.
pub fn clipboard_commands() -> &'static [(&'static str, &'static [&'static str])] {
    #[cfg(target_os = "macos")]
    {
        const MACOS: &[(&str, &[&str])] = &[("pbcopy", &[])];
        MACOS
    }
    #[cfg(target_os = "windows")]
    {
        const WINDOWS: &[(&str, &[&str])] = &[("clip", &[])];
        WINDOWS
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Wayland first, then X11's two usual suspects.
        const UNIX: &[(&str, &[&str])] = &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        UNIX
    }
}

/// Copy `text` to the system clipboard.
pub fn copy(text: &str) -> io::Result<()> {
    let mut last_error = None;
    for (program, args) in clipboard_commands() {
        let spawned = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                let written = match child.stdin.as_mut() {
                    Some(stdin) => stdin.write_all(text.as_bytes()),
                    None => Ok(()),
                };
                drop(child.stdin.take());
                // Reap the child even when the write failed, so it never
                // lingers as a zombie.
                let status = child.wait();
                written?;
                return status.map(|_| ());
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                last_error = Some(err);
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "no clipboard tool is installed")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_list_is_platform_appropriate() {
        let commands = clipboard_commands();
        assert!(!commands.is_empty());
        #[cfg(target_os = "macos")]
        assert_eq!(commands[0].0, "pbcopy");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            assert_eq!(commands[0].0, "wl-copy");
            assert!(commands.iter().any(|(program, _)| *program == "xclip"));
        }
    }
}
