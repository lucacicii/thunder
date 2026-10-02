//! Regression: constructing a store for a *fresh* root from several tasks at
//! once used to fail with a transient `NotFound`. The TUI and the daemon (and
//! the daemon's parallel tests) share `~/.thunder/conversations`, so a loser of
//! the `mkdir` / `read_dir` race must retry, not error.

use std::sync::Arc;

use thunder_conversation::prelude::FsConversationStore;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_new_on_a_clean_root_succeeds() {
    for _ in 0..100 {
        let dir = tempfile::tempdir().unwrap();
        let root = Arc::new(dir.path().join("conversations"));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let root = Arc::clone(&root);
            handles.push(tokio::spawn(async move {
                FsConversationStore::new(root.as_ref().clone()).await
            }));
        }
        for handle in handles {
            let result = handle.await.unwrap();
            assert!(
                result.is_ok(),
                "FsConversationStore::new failed: {:?}",
                result.err()
            );
        }
    }
}
