//! Additional directories reach the search tools.
//!
//! `extra_workspace_dirs` used to be declared on `run_task` and read by nobody, so a
//! host could put a second repository in scope and watch the agent fail to find it.
//! The scope travels on `_meta.workspace_dirs`; these tests pin what that means:
//! search widens, relative paths and writes do not.

use serde_json::json;
use tempfile::tempdir;
use thunder_protocol::McpToolCallParams;
use thunder_std::dispatch_tool_call;

fn call(name: &str, args: serde_json::Value, cwd: &std::path::Path, dirs: Vec<String>) -> String {
    let params: McpToolCallParams = serde_json::from_value(json!({
        "name": name,
        "arguments": args,
        "_meta": {
            "session_id": "sess-multi",
            "workspace_cwd": cwd.to_string_lossy(),
            "timeout_ms": 30_000,
            "workspace_dirs": dirs,
        }
    }))
    .expect("params deserialize");

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(dispatch_tool_call(params))
        .combined_text()
}

#[test]
fn grep_without_a_path_walks_every_root() {
    let primary = tempdir().unwrap();
    let extra = tempdir().unwrap();
    std::fs::write(primary.path().join("a.txt"), "needle-in-primary").unwrap();
    std::fs::write(extra.path().join("b.txt"), "needle-in-extra").unwrap();

    let out = call(
        "grep",
        json!({ "pattern": "needle" }),
        primary.path(),
        vec![extra.path().to_string_lossy().to_string()],
    );

    assert!(out.contains("needle-in-primary"), "{out}");
    assert!(
        out.contains("needle-in-extra"),
        "an additional root must be searched: {out}"
    );
    // Both sections are labelled, so a hit can be attributed to a directory.
    assert!(out.contains("=="), "{out}");
}

#[test]
fn an_explicit_path_still_means_exactly_that_path() {
    let primary = tempdir().unwrap();
    let extra = tempdir().unwrap();
    std::fs::write(primary.path().join("a.txt"), "needle-in-primary").unwrap();
    std::fs::write(extra.path().join("b.txt"), "needle-in-extra").unwrap();

    let out = call(
        "grep",
        json!({ "pattern": "needle", "path": "." }),
        primary.path(),
        vec![extra.path().to_string_lossy().to_string()],
    );

    assert!(out.contains("needle-in-primary"), "{out}");
    assert!(
        !out.contains("needle-in-extra"),
        "an explicit path must not be widened: {out}"
    );
}

#[test]
fn find_walks_every_root_but_ls_is_a_directory_listing_each() {
    let primary = tempdir().unwrap();
    let extra = tempdir().unwrap();
    std::fs::write(primary.path().join("only-primary.md"), "x").unwrap();
    std::fs::write(extra.path().join("only-extra.md"), "x").unwrap();

    let dirs = vec![extra.path().to_string_lossy().to_string()];

    let found = call(
        "find",
        json!({ "pattern": "*.md" }),
        primary.path(),
        dirs.clone(),
    );
    assert!(found.contains("only-primary.md"), "{found}");
    assert!(found.contains("only-extra.md"), "{found}");

    let listed = call("ls", json!({}), primary.path(), dirs);
    assert!(listed.contains("only-primary.md"), "{listed}");
    assert!(listed.contains("only-extra.md"), "{listed}");
}

#[test]
fn relative_paths_still_belong_to_the_primary_root() {
    let primary = tempdir().unwrap();
    let extra = tempdir().unwrap();
    std::fs::write(extra.path().join("shadow.txt"), "in-extra").unwrap();

    // `read_file` resolves against the primary root, so a name that only exists in an
    // additional directory must not resolve: search widened, interpretation did not.
    let out = call(
        "read_file",
        json!({ "path": "shadow.txt" }),
        primary.path(),
        vec![extra.path().to_string_lossy().to_string()],
    );
    assert!(
        !out.contains("in-extra"),
        "relative paths must stay primary: {out}"
    );
}

#[test]
fn a_single_root_keeps_the_old_output_shape() {
    let primary = tempdir().unwrap();
    std::fs::write(primary.path().join("only-here.txt"), "needle").unwrap();

    let out = call(
        "grep",
        json!({ "pattern": "needle" }),
        primary.path(),
        Vec::new(),
    );
    assert!(out.contains("only-here.txt"), "{out}");
    assert!(
        !out.contains("=="),
        "with one root there is nothing to label: {out}"
    );
}
