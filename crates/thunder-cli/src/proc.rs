//! Live daemon discovery, the runtime state marker, and the `SIGUSR1` reload signal.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use crate::paths;

#[derive(Debug, Clone)]
pub struct RunningDaemon {
    pub pid: u32,
    pub exe: PathBuf,
    pub is_script: bool,
}

impl RunningDaemon {
    pub fn state_file(&self) -> PathBuf {
        paths::runtime_state_file(self.pid)
    }

    /// Runtime marker written by `thunder-daemon` at startup, if it supports reload.
    pub fn read_state(&self) -> Option<Value> {
        let text = std::fs::read_to_string(self.state_file()).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// Every live `thunder-daemon` process (installed, repo-built, or bundled).
pub fn running_daemons() -> Vec<RunningDaemon> {
    let output = match Command::new("ps").args(["-axo", "pid=,command="]).output() {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let mut parts = line.splitn(2, char::is_whitespace);
        let Some(pid) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        let Some(command) = parts.next().map(str::trim).filter(|c| !c.is_empty()) else {
            continue;
        };
        let exe_token = command.split_whitespace().next().unwrap_or_default();
        let basename = exe_token.rsplit('/').next().unwrap_or(exe_token);

        let is_binary = basename == "thunder-daemon";
        let is_script = command.contains("daemon.sh");
        if !is_binary && !is_script {
            continue;
        }
        // Skip the shell wrapper `daemon.sh` leaves behind while cargo is building.
        if std::process::id() == pid {
            continue;
        }

        let exe = if is_binary {
            PathBuf::from(exe_token)
        } else {
            PathBuf::from(
                command
                    .split_whitespace()
                    .find(|t| t.ends_with("daemon.sh"))
                    .unwrap_or_default(),
            )
        };
        out.push(RunningDaemon {
            pid,
            exe,
            is_script,
        });
    }
    out
}

/// Send `SIGUSR1` to ask the daemon to re-exec its own binary in place.
///
/// Only ever called after [`RunningDaemon::read_state`] confirmed the daemon
/// advertises `hot_reload`, because `SIGUSR1`'s default disposition is *terminate*.
pub fn request_hot_reload(pid: u32) -> Result<(), String> {
    let status = Command::new("kill")
        .arg("-USR1")
        .arg(pid.to_string())
        .status()
        .map_err(|e| format!("failed to run kill: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill -USR1 {pid} exited with {status}"))
    }
}

/// Unix epoch milliseconds, or 0 when the platform clock is unavailable.
pub fn mtime_ms(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
