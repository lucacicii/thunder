//! The mode commands over the real NDJSON transport.
//!
//! `ModePlugin` is unit-tested to death; what is only true here is that a host
//! can *reach* it from outside the process. That is the whole point of D6 —
//! the TUI hardcoded `policy: None` before 0.4, so a mode the model was judged
//! by was one no user could set. Reaching it through a JSON frame, on the wire
//! a host actually speaks, is the property this file pins.

use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

/// One daemon for the whole file: three assertions on the same process prove
/// the mode is *remembered* per session, which a process per case would not.
async fn ask(
    stdin: &mut ChildStdin,
    reader: &mut Lines<BufReader<ChildStdout>>,
    request: serde_json::Value,
) -> serde_json::Value {
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .expect("write request");
    stdin.flush().await.expect("flush request");
    let line = reader
        .next_line()
        .await
        .expect("read response")
        .expect("the daemon answered");
    serde_json::from_str(&line).expect("a response frame")
}

/// The next frame, or `None` when the daemon stays quiet for `ms`.
///
/// A pushed frame is not an answer to anything, so it cannot be read with
/// [`ask`]: it arrives after the response it accompanied, and a client that
/// only reads one line per request would leave it queued for the *next* read.
async fn next_frame(
    reader: &mut Lines<BufReader<ChildStdout>>,
    ms: u64,
) -> Option<serde_json::Value> {
    let line = tokio::time::timeout(Duration::from_millis(ms), reader.next_line())
        .await
        .ok()?
        .expect("read")
        .filter(|l| !l.trim().is_empty())?;
    Some(serde_json::from_str(&line).expect("a frame"))
}

#[tokio::test]
async fn a_host_can_read_and_set_the_mode_over_the_wire() -> Result<(), Box<dyn std::error::Error>>
{
    let real_home = tempfile::tempdir()?;
    let data_root = tempfile::tempdir()?;

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_thunder-daemon"))
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

    // A session nobody has configured runs in the default mode. Worth pinning:
    // `Mode::default()` is Yolo for pre-0.4 continuity, and if that ever drifts
    // to a fail-closed mode, headless hosts stop working silently.
    let state = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "get_permission_state",
            "id": "req-state-1",
            "session_id": "s-mode",
        }),
    )
    .await;
    assert_eq!(state["success"], true, "{state}");
    assert_eq!(state["data"]["mode"], "yolo", "{state}");

    // Set it.
    let set = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "set_mode",
            "id": "req-set-1",
            "session_id": "s-mode",
            "mode": "plan",
        }),
    )
    .await;
    assert_eq!(set["success"], true, "{set}");
    assert_eq!(set["data"]["mode"], "plan", "{set}");

    // The change is *pushed*, not only answered: a panel that never asked can
    // see the mode move. The response came first (that ordering is what keeps a
    // one-line-per-request client reading the right frame), so the announcement
    // is the next line.
    let pushed = next_frame(&mut reader, 2_000)
        .await
        .expect("a mode change is announced, not just answered");
    assert_eq!(pushed["type"], "mode_changed", "{pushed}");
    assert_eq!(pushed["session_id"], "s-mode", "{pushed}");
    assert_eq!(pushed["mode"], "plan", "{pushed}");
    assert_eq!(
        pushed["previous"], "yolo",
        "the transition is carried: {pushed}"
    );
    assert_eq!(pushed["source"], "user", "{pushed}");
    assert!(
        pushed.get("task_id").is_none(),
        "nothing is running, so there is no task to name: {pushed}"
    );

    // And it stuck — for that session.
    let state = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "get_permission_state",
            "id": "req-state-2",
            "session_id": "s-mode",
        }),
    )
    .await;
    assert_eq!(state["data"]["mode"], "plan", "{state}");

    // A different session is a different judgment: one session's mode must not
    // decide what another session may do.
    let other = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "get_permission_state",
            "id": "req-state-3",
            "session_id": "s-other",
        }),
    )
    .await;
    assert_eq!(other["data"]["mode"], "yolo", "{other}");

    // The pre-0.4 names still resolve, because a host pinned to them has no way
    // to know they were renamed.
    let legacy = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "set_mode",
            "id": "req-set-2",
            "session_id": "s-legacy",
            "mode": "accept_edits",
        }),
    )
    .await;
    assert_eq!(legacy["success"], true, "{legacy}");
    assert_eq!(legacy["data"]["mode"], "acceptEdits", "{legacy}");

    // An unknown mode is an error, never a silent fallback: a typo must not
    // quietly leave the session in whatever the default was.
    let bad = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "set_mode",
            "id": "req-set-3",
            "session_id": "s-mode",
            "mode": "no-such-mode",
        }),
    )
    .await;
    assert_eq!(bad["success"], false, "{bad}");
    assert!(
        bad["error"].as_str().unwrap_or_default().contains("plan"),
        "the error names the valid modes: {bad}"
    );

    drop(stdin);
    let _ = child.kill().await;
    let _ = child.wait().await;
    Ok(())
}

/// Silence is the default: a `set_mode` that changes nothing must not announce
/// anything.
///
/// This is the property that keeps the event worth listening to. The host
/// re-applies a run's mode at every run start, so if "asked for the mode it is
/// already in" were announced, every turn would produce a frame and a panel
/// would learn to ignore them.
#[tokio::test]
async fn a_mode_that_does_not_change_is_not_announced() -> Result<(), Box<dyn std::error::Error>> {
    let real_home = tempfile::tempdir()?;
    let data_root = tempfile::tempdir()?;

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_thunder-daemon"))
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

    // Reading a session's state creates it — in the default mode. Asking again
    // for that same mode is not a change.
    let state = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "get_permission_state", "id": "req-1", "session_id": "s-same",
        }),
    )
    .await;
    assert_eq!(state["data"]["mode"], "yolo", "{state}");

    let same = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "set_mode", "id": "req-2", "session_id": "s-same", "mode": "yolo",
        }),
    )
    .await;
    assert_eq!(same["success"], true, "{same}");
    assert!(
        next_frame(&mut reader, 400).await.is_none(),
        "re-applying the current mode says nothing"
    );

    // A session that does not exist yet is created *in* the requested mode, so
    // there is no transition to report — the response is the whole answer.
    let fresh = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "set_mode", "id": "req-3", "session_id": "s-fresh", "mode": "plan",
        }),
    )
    .await;
    assert_eq!(fresh["data"]["mode"], "plan", "{fresh}");
    assert!(
        next_frame(&mut reader, 400).await.is_none(),
        "creating a session in a mode is not a change of mode"
    );

    // A real change on a session that already existed is announced.
    let moved = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "set_mode", "id": "req-4", "session_id": "s-fresh", "mode": "ask",
        }),
    )
    .await;
    assert_eq!(moved["data"]["mode"], "ask", "{moved}");
    let pushed = next_frame(&mut reader, 2_000)
        .await
        .expect("a real change is announced");
    assert_eq!(pushed["type"], "mode_changed", "{pushed}");
    assert_eq!(pushed["session_id"], "s-fresh", "{pushed}");
    assert_eq!(pushed["mode"], "ask", "{pushed}");
    assert_eq!(pushed["previous"], "plan", "{pushed}");
    assert_eq!(pushed["source"], "user", "{pushed}");

    // A rejected mode is an error and changes nothing, so it announces nothing.
    let bad = ask(
        &mut stdin,
        &mut reader,
        serde_json::json!({
            "method": "set_mode", "id": "req-5", "session_id": "s-fresh", "mode": "nope",
        }),
    )
    .await;
    assert_eq!(bad["success"], false, "{bad}");
    assert!(
        next_frame(&mut reader, 400).await.is_none(),
        "a refused mode must not look like a change"
    );

    drop(stdin);
    let _ = child.kill().await;
    let _ = child.wait().await;
    Ok(())
}
