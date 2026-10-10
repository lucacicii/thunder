//! The decisive gate for thunder's data-root contract: a host that names its own
//! root (`THUNDER_CONFIG_DIR`, or the `--config-dir` flag) must never touch the
//! end user's `~/.thunder`.
//!
//! The real binary is spawned out-of-process with `HOME` pointed at a temp dir
//! and the root at another, so `$HOME` is a lie the process cannot escape: the
//! assertion is a filesystem walk of the fake home, not a reading of the env.
//!
//! No `testing-mock` feature is needed. The conversation store is opened during
//! `DaemonService::new` (`service.rs:133-134`) — before any request arrives — so
//! a single `ping` proves the process booted against the injected root.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[tokio::test]
async fn daemon_writes_only_under_the_injected_root() -> Result<(), Box<dyn std::error::Error>> {
    let real_home = tempfile::tempdir()?;
    let data_root = tempfile::tempdir()?;

    let mut child = Command::new(env!("CARGO_BIN_EXE_thunder-daemon"))
        .env("HOME", real_home.path())
        .env_remove("USERPROFILE")
        .env("THUNDER_CONFIG_DIR", data_root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout).lines();

    let ping = serde_json::json!({ "method": "ping", "id": "req-ping-1" });
    stdin.write_all(format!("{ping}\n").as_bytes()).await?;
    stdin.flush().await?;

    let line = reader
        .next_line()
        .await?
        .expect("expected a ping response: the daemon should be up");
    let resp: serde_json::Value = serde_json::from_str(&line)?;
    assert_eq!(resp["success"], true, "daemon should answer ping: {resp}");

    drop(stdin);
    let _ = child.kill().await;
    let _ = child.wait().await;

    // A live write into the injected root: the store directory is created when
    // the daemon opens the store at startup.
    assert!(
        data_root.path().join("conversations").is_dir(),
        "expected the conversation store under the injected root {data_root:?}, found {:#?}",
        list_dir(data_root.path())
    );

    // ...and the user's home must be untouched. This is the whole contract.
    assert!(
        !real_home.path().join(".thunder").exists(),
        "the daemon created $HOME/.thunder despite an injected root: {:#?}",
        list_dir(real_home.path())
    );
    assert!(
        list_dir(real_home.path()).is_empty(),
        "nothing should appear in the user's home directory at all: {:#?}",
        list_dir(real_home.path())
    );

    Ok(())
}

/// An explicit flag must beat an inherited root: a host that passes
/// `--config-dir` should not be silently overridden by its own environment.
#[tokio::test]
async fn flag_beats_inherited_root() -> Result<(), Box<dyn std::error::Error>> {
    let real_home = tempfile::tempdir()?;
    let env_root = tempfile::tempdir()?;
    let flag_root = tempfile::tempdir()?;

    let mut child = Command::new(env!("CARGO_BIN_EXE_thunder-daemon"))
        .env("HOME", real_home.path())
        .env_remove("USERPROFILE")
        .env("THUNDER_CONFIG_DIR", env_root.path())
        .arg("--config-dir")
        .arg(flag_root.path())
        // Piped and left open, so the child stays alive while we poll.
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut appeared = false;
    while Instant::now() < deadline {
        if flag_root.path().join("conversations").is_dir() {
            appeared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(
        appeared,
        "the flag root never received the store: {:#?}",
        list_dir(flag_root.path())
    );
    assert!(
        !env_root.path().join("conversations").exists(),
        "the inherited root should have been replaced by the flag, not merged with it: {:#?}",
        list_dir(env_root.path())
    );
    assert!(
        !real_home.path().join(".thunder").exists(),
        "the user's home must stay untouched: {:#?}",
        list_dir(real_home.path())
    );

    Ok(())
}

fn list_dir(dir: &Path) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    entries
}
