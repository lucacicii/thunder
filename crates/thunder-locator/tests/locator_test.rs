use std::path::PathBuf;
use tempfile::TempDir;
use thunder_locator::{
    CandidateSource, LocateVerdict, MemoryBridge, WorkspaceLocator, WorkspaceRecord,
    WorkspaceRegistry,
};

#[tokio::test]
async fn test_locator_direct_match_and_alias() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let dir_a = root.join("anfeng");
    let dir_b = root.join("agent-resume-panel");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    let json_path = root.join("workspaces.json");
    let dummy_db = root.join("memory.db");
    let bridge = MemoryBridge::new(dummy_db);
    let registry = WorkspaceRegistry::load_or_init(json_path, bridge.clone()).await;

    registry
        .bind_or_update(
            dir_a.clone(),
            Some("anfeng".to_string()),
            vec!["安丰仓储".to_string(), "anfeng-web".to_string()],
            vec!["运单".to_string(), "磅单".to_string()],
            vec!["vue".to_string(), "spring".to_string()],
        )
        .await;

    registry
        .bind_or_update(
            dir_b.clone(),
            Some("agent-resume-panel".to_string()),
            vec!["桌面面板".to_string()],
            vec!["composer".to_string(), "workbench".to_string()],
            vec!["electron".to_string(), "rust".to_string()],
        )
        .await;

    let locator = WorkspaceLocator::new(registry, bridge);

    // 1. Exact name match from unrelated dir
    let unrelated = root.join("unrelated");
    std::fs::create_dir_all(&unrelated).unwrap();

    let verdict1 = locator.locate("请帮忙修复 anfeng 的登录接口", &unrelated).await;
    match verdict1 {
        LocateVerdict::DirectMatch { target_path, .. } => {
            assert_eq!(target_path.canonicalize().unwrap(), dir_a.canonicalize().unwrap());
        }
        other => panic!("Expected DirectMatch, got {:?}", other),
    }

    // 2. Alias match
    let verdict2 = locator.locate("重构安丰仓储的报表统计", &unrelated).await;
    match verdict2 {
        LocateVerdict::DirectMatch { target_path, .. } => {
            assert_eq!(target_path.canonicalize().unwrap(), dir_a.canonicalize().unwrap());
        }
        other => panic!("Expected DirectMatch, got {:?}", other),
    }

    // 3. Keyword match for agent-resume-panel
    let verdict3 = locator.locate("修复 composer 窗口无法聚焦的问题", &unrelated).await;
    match verdict3 {
        LocateVerdict::DirectMatch { target_path, .. } => {
            assert_eq!(target_path.canonicalize().unwrap(), dir_b.canonicalize().unwrap());
        }
        other => panic!("Expected DirectMatch, got {:?}", other),
    }
}

#[tokio::test]
async fn test_locator_already_in_workspace() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let dir_a = root.join("anfeng");
    std::fs::create_dir_all(&dir_a).unwrap();

    let json_path = root.join("workspaces.json");
    let dummy_db = root.join("memory.db");
    let bridge = MemoryBridge::new(dummy_db);
    let registry = WorkspaceRegistry::load_or_init(json_path, bridge.clone()).await;

    registry
        .bind_or_update(
            dir_a.clone(),
            Some("anfeng".to_string()),
            vec!["安丰".to_string()],
            vec!["运单".to_string()],
            vec![],
        )
        .await;

    let locator = WorkspaceLocator::new(registry, bridge);

    // Prompt targets anfeng while currently already inside anfeng
    let verdict = locator.locate("查看 anfeng 的配置", &dir_a).await;
    match verdict {
        LocateVerdict::AlreadyInWorkspace { current_path, .. } => {
            assert_eq!(current_path.canonicalize().unwrap(), dir_a.canonicalize().unwrap());
        }
        other => panic!("Expected AlreadyInWorkspace, got {:?}", other),
    }
}

#[tokio::test]
async fn test_locator_no_memory_and_fast_tagging() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let current_dir = root.join("my-new-crawler");
    std::fs::create_dir_all(&current_dir).unwrap();

    let json_path = root.join("workspaces.json");
    let dummy_db = root.join("memory.db");
    let bridge = MemoryBridge::new(dummy_db);
    let registry = WorkspaceRegistry::load_or_init(json_path, bridge.clone()).await;
    let locator = WorkspaceLocator::new(registry.clone(), bridge);

    // Prompt has zero prior memory
    let prompt = "实现一个巨潮网定期财报采集功能";
    let verdict = locator.locate(prompt, &current_dir).await;

    let candidates = match verdict {
        LocateVerdict::NeedConfirmation { candidates, .. } => candidates,
        other => panic!("Expected NeedConfirmation, got {:?}", other),
    };

    // Candidate 1 must be current workspace (Option 3: Fast Tagging)
    assert!(!candidates.is_empty());
    assert_eq!(
        candidates[0].path.canonicalize().unwrap(),
        current_dir.canonicalize().unwrap()
    );
    assert_eq!(candidates[0].source, CandidateSource::CurrentWorkspace);

    // User chooses Option 1: Bind current workspace with keywords
    registry
        .bind_or_update(
            current_dir.clone(),
            Some("my-new-crawler".to_string()),
            vec!["财报采集器".to_string()],
            vec!["巨潮网".to_string(), "财报".to_string()],
            vec!["python".to_string()],
        )
        .await;

    // Next time from another directory, it can now be directly located!
    let other_dir = root.join("other");
    std::fs::create_dir_all(&other_dir).unwrap();

    let verdict2 = locator.locate("优化巨潮网财报解析逻辑", &other_dir).await;
    match verdict2 {
        LocateVerdict::DirectMatch { target_path, .. } => {
            assert_eq!(
                target_path.canonicalize().unwrap(),
                current_dir.canonicalize().unwrap()
            );
        }
        other => panic!("Expected DirectMatch after binding, got {:?}", other),
    }
}

#[tokio::test]
async fn test_locator_nonexistent_directory_safety() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let dead_dir = root.join("deleted_project");
    // Do NOT create the directory on disk!

    let json_path = root.join("workspaces.json");
    let dummy_db = root.join("memory.db");
    let bridge = MemoryBridge::new(dummy_db);
    let registry = WorkspaceRegistry::load_or_init(json_path, bridge.clone()).await;

    registry
        .bind_or_update(
            dead_dir.clone(),
            Some("deleted_project".to_string()),
            vec!["不存在的项目".to_string()],
            vec!["dead".to_string()],
            vec![],
        )
        .await;

    let locator = WorkspaceLocator::new(registry, bridge);
    let curr = root.join("current");
    std::fs::create_dir_all(&curr).unwrap();

    // Since the dead_dir does not exist on disk, it must NOT be directly switched to
    let verdict = locator.locate("打开不存在的项目", &curr).await;
    match verdict {
        LocateVerdict::DirectMatch { target_path, .. } => {
            panic!("Must not direct match a non-existent directory: {:?}", target_path);
        }
        LocateVerdict::NeedConfirmation { candidates, .. } => {
            assert!(candidates.iter().all(|c| c.path.exists()));
        }
        LocateVerdict::AlreadyInWorkspace { .. } => {}
    }
}

#[tokio::test]
async fn test_locator_with_thunder_memory_sqlite() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let proj_dir = root.join("sdsteel");
    std::fs::create_dir_all(&proj_dir).unwrap();

    let db_path = root.join("memory.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE composer_training_samples (
            id TEXT PRIMARY KEY,
            project_path TEXT NOT NULL,
            raw_text TEXT NOT NULL,
            extracted_entities TEXT
        );
        "#,
    )
    .unwrap();

    let dir_str = proj_dir.to_string_lossy().to_string();
    conn.execute(
        "INSERT INTO composer_training_samples (id, project_path, raw_text, extracted_entities)
         VALUES ('1', ?1, '山钢大屏可视化图表修改', '[\"山钢\", \"可视化\"]')",
        [&dir_str],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO composer_training_samples (id, project_path, raw_text, extracted_entities)
         VALUES ('2', ?1, '山钢接口鉴权 token 刷新', '[\"山钢\", \"鉴权\"]')",
        [&dir_str],
    )
    .unwrap();
    drop(conn);

    let bridge = MemoryBridge::new(db_path);
    let json_path = root.join("workspaces.json");
    let registry = WorkspaceRegistry::load_or_init(json_path, bridge.clone()).await;
    let locator = WorkspaceLocator::new(registry, bridge).with_threshold(0.20);

    let other_dir = root.join("somewhere");
    std::fs::create_dir_all(&other_dir).unwrap();

    let verdict = locator.locate("修改山钢的大屏看板", &other_dir).await;
    match verdict {
        LocateVerdict::DirectMatch { target_path, .. } => {
            assert_eq!(target_path.canonicalize().unwrap(), proj_dir.canonicalize().unwrap());
        }
        other => panic!("Expected DirectMatch from thunder-memory, got {:?}", other),
    }
}
