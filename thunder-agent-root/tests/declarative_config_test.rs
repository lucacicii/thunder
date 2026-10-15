use std::fs;
use tempfile::tempdir;
use thunder_agent_root::loader::PluginLoader;
use thunder_agent_root::thunder_config::ThunderConfig;

#[tokio::test]
async fn test_declarative_config_builtin_filter() {
    let json_str = r#"{
        "plugins": {
            "builtin": ["read_file", "write_file"]
        }
    }"#;
    let cfg: ThunderConfig = serde_json::from_str(json_str).unwrap();
    let loaded = PluginLoader::load_plugins(&cfg, None, true).await;

    let ids: Vec<&str> = loaded.tool_plugins.iter().map(|p| p.id()).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"read_file"));
    assert!(ids.contains(&"write_file"));
    assert!(!ids.contains(&"bash"));
    assert!(!ids.contains(&"grep"));
}

#[tokio::test]
async fn test_declarative_config_disabled_filter() {
    let json_str = r#"{
        "plugins": {
            "disabled": ["bash", "ls"]
        }
    }"#;
    let cfg: ThunderConfig = serde_json::from_str(json_str).unwrap();
    let loaded = PluginLoader::load_plugins(&cfg, None, true).await;

    let ids: Vec<&str> = loaded.tool_plugins.iter().map(|p| p.id()).collect();
    assert!(!ids.contains(&"bash"));
    assert!(!ids.contains(&"ls"));
    assert!(ids.contains(&"read_file"));
    assert!(ids.contains(&"write_file"));
    assert!(ids.contains(&"grep"));
    assert!(ids.contains(&"find"));
}

#[tokio::test]
async fn test_declarative_config_workspace_file_loading() {
    let dir = tempdir().unwrap();
    let thunder_dir = dir.path().join(".thunder");
    fs::create_dir_all(&thunder_dir).unwrap();

    let config_content = r#"{
        "plugins": {
            "disabled": ["bash"]
        }
    }"#;
    fs::write(thunder_dir.join("config.json"), config_content).unwrap();

    let cfg = ThunderConfig::load(Some(dir.path())).await;
    let loaded = PluginLoader::load_plugins(&cfg, Some(dir.path()), true).await;

    let ids: Vec<&str> = loaded.tool_plugins.iter().map(|p| p.id()).collect();
    assert!(!ids.contains(&"bash"));
    assert!(ids.contains(&"read_file"));
    assert!(ids.contains(&"write_file"));
}
