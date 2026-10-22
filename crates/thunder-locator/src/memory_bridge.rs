use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::debug;

/// Bridge to the external thunder-memory SQLite database (~/.thunder/memory.db).
#[derive(Clone, Debug)]
pub struct MemoryBridge {
    db_path: PathBuf,
}

impl MemoryBridge {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    pub fn default_db_path() -> PathBuf {
        thunder_protocol::thunder_config_dir().join("memory.db")
    }

    pub fn is_available(&self) -> bool {
        self.db_path.exists()
    }

    fn open_read_only(&self) -> Option<Connection> {
        if !self.db_path.exists() {
            return None;
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        Connection::open_with_flags(&self.db_path, flags).ok()
    }

    /// Query the top active project paths from historical composer training samples.
    /// Only returns paths that actually exist on the local filesystem.
    pub fn query_top_project_paths(&self, limit: usize) -> Vec<(PathBuf, usize)> {
        let Some(conn) = self.open_read_only() else {
            return Vec::new();
        };

        let mut stmt = match conn.prepare(
            "SELECT project_path, count(*) as c 
             FROM composer_training_samples 
             WHERE project_path IS NOT NULL AND length(project_path) > 1
             GROUP BY project_path 
             ORDER BY c DESC 
             LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(e) => {
                debug!("Failed to query composer_training_samples: {e}");
                return Vec::new();
            }
        };

        let rows = stmt.query_map([limit as i64 * 3], |row| {
            let path_str: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((path_str, count as usize))
        });

        let mut results = Vec::new();
        if let Ok(mapped) = rows {
            for item in mapped.flatten() {
                let p = PathBuf::from(&item.0);
                if p.is_dir() {
                    results.push((p, item.1));
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }
        results
    }

    /// Query project paths whose historical prompts or entities match any of the given keywords.
    pub fn search_project_associations(&self, keywords: &[String], limit: usize) -> Vec<(PathBuf, usize)> {
        if keywords.is_empty() {
            return Vec::new();
        }
        let Some(conn) = self.open_read_only() else {
            return Vec::new();
        };

        let mut path_scores: HashMap<PathBuf, usize> = HashMap::new();

        // Search in composer_training_samples
        for kw in keywords {
            if kw.trim().is_empty() {
                continue;
            }
            let pattern = format!("%{kw}%");
            let mut stmt = match conn.prepare(
                "SELECT project_path, count(*) 
                 FROM composer_training_samples 
                 WHERE (raw_text LIKE ?1 OR extracted_entities LIKE ?1)
                   AND project_path IS NOT NULL AND length(project_path) > 1
                 GROUP BY project_path 
                 LIMIT 20",
            ) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let rows_result = stmt.query_map([&pattern], |row| {
                let path_str: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((path_str, count as usize))
            });

            if let Ok(mapped) = rows_result {
                let matched_items: Vec<(String, usize)> = mapped.flatten().collect();
                for (path_str, count) in matched_items {
                    let p = PathBuf::from(path_str);
                    if p.is_dir() {
                        *path_scores.entry(p).or_insert(0) += count;
                    }
                }
            }
        }

        let mut sorted: Vec<(PathBuf, usize)> = path_scores.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(limit);
        sorted
    }
}
