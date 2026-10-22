use crate::memory_bridge::MemoryBridge;
use crate::model::WorkspaceRecord;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceCatalog {
    pub version: u32,
    pub workspaces: Vec<WorkspaceRecord>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceRegistry {
    storage_path: PathBuf,
    catalog: Arc<RwLock<WorkspaceCatalog>>,
    memory_bridge: MemoryBridge,
}

impl WorkspaceRegistry {
    pub async fn load_or_init(storage_path: PathBuf, memory_bridge: MemoryBridge) -> Self {
        let catalog = if storage_path.exists() {
            match tokio::fs::read_to_string(&storage_path).await {
                Ok(content) => {
                    serde_json::from_str::<WorkspaceCatalog>(&content).unwrap_or_else(|e| {
                        debug!(
                            "Failed to parse {}: {e}, starting fresh",
                            storage_path.display()
                        );
                        WorkspaceCatalog {
                            version: 1,
                            workspaces: Vec::new(),
                        }
                    })
                }
                Err(_) => WorkspaceCatalog {
                    version: 1,
                    workspaces: Vec::new(),
                },
            }
        } else {
            let mut cat = WorkspaceCatalog {
                version: 1,
                workspaces: Vec::new(),
            };
            // Seed from top active paths in thunder-memory if available
            let top_paths = memory_bridge.query_top_project_paths(5);
            for (p, hits) in top_paths {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    let mut rec = WorkspaceRecord::new(p.clone(), name);
                    rec.hit_count = hits as u64;
                    cat.workspaces.push(rec);
                }
            }
            cat
        };

        let registry = Self {
            storage_path,
            catalog: Arc::new(RwLock::new(catalog)),
            memory_bridge,
        };

        // Persist initial seed if created
        let _ = registry.save().await;
        registry
    }

    pub fn memory_bridge(&self) -> &MemoryBridge {
        &self.memory_bridge
    }

    pub fn default_storage_path() -> PathBuf {
        thunder_protocol::thunder_config_dir().join("workspaces.json")
    }

    pub async fn list(&self) -> Vec<WorkspaceRecord> {
        self.catalog.read().await.workspaces.clone()
    }

    pub async fn find_by_path(&self, path: &Path) -> Option<WorkspaceRecord> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let cat = self.catalog.read().await;
        cat.workspaces
            .iter()
            .find(|w| w.canonical_path == canonical || w.canonical_path == path)
            .cloned()
    }

    pub async fn bind_or_update(
        &self,
        path: PathBuf,
        name: Option<String>,
        aliases: Vec<String>,
        keywords: Vec<String>,
        tech_stack: Vec<String>,
    ) -> WorkspaceRecord {
        let canonical = path.canonicalize().unwrap_or(path.clone());
        let fallback_name = canonical
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("workspace")
            .to_string();
        let final_name = name.unwrap_or(fallback_name);

        let mut cat = self.catalog.write().await;
        let mut target_idx = None;
        for (i, w) in cat.workspaces.iter().enumerate() {
            if w.canonical_path == canonical {
                target_idx = Some(i);
                break;
            }
        }

        let rec = if let Some(idx) = target_idx {
            let item = &mut cat.workspaces[idx];
            item.name = final_name;
            for a in aliases {
                if !item.aliases.contains(&a) && a != item.name {
                    item.aliases.push(a);
                }
            }
            for k in keywords {
                if !item.keywords.contains(&k) {
                    item.keywords.push(k);
                }
            }
            for t in tech_stack {
                if !item.tech_stack.contains(&t) {
                    item.tech_stack.push(t);
                }
            }
            item.last_used_ms = now_ms();
            item.clone()
        } else {
            let mut item = WorkspaceRecord::new(canonical, final_name)
                .with_aliases(aliases)
                .with_keywords(keywords)
                .with_tech_stack(tech_stack);
            item.last_used_ms = now_ms();
            cat.workspaces.push(item.clone());
            item
        };

        drop(cat);
        let _ = self.save().await;
        rec
    }

    pub async fn record_hit(&self, path: &Path) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut cat = self.catalog.write().await;
        if let Some(item) = cat
            .workspaces
            .iter_mut()
            .find(|w| w.canonical_path == canonical)
        {
            item.hit_count += 1;
            item.last_used_ms = now_ms();
        }
        drop(cat);
        let _ = self.save().await;
    }

    pub async fn save(&self) -> Result<(), std::io::Error> {
        let cat = self.catalog.read().await;
        if let Some(parent) = self.storage_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let serialized = serde_json::to_string_pretty(&*cat).map_err(std::io::Error::other)?;
        tokio::fs::write(&self.storage_path, serialized).await
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
