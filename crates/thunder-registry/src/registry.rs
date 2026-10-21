use crate::frontmatter::scan_scripts_dir;
use arc_swap::ArcSwap;
use parking_lot::RwLock;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use thunder_executor::ScriptToolProvider;
use thunder_protocol::{ExecutionContext, ToolDescriptor, ToolExecutionError, ToolProvider};
use tracing::warn;

/// Immutable point-in-time snapshot of all active tools and their providers (§4.1, §6.2).
pub struct CatalogSnapshot {
    pub version: u64,
    /// Deterministically ordered (`BTreeMap`) descriptors keyed by exposed name.
    pub descriptors: BTreeMap<String, ToolDescriptor>,
    /// Routing table mapping exposed tool name, logical name, and URI to the winning `ToolProvider`.
    pub routing_table: HashMap<String, Arc<dyn ToolProvider>>,
    /// Logical tool name -> winning exposed tool name (after shadowing resolution).
    pub logical_winners: HashMap<String, String>,
    /// URIs of tools shadowed due to lower namespace priority (`Script > MCP > Core`).
    pub shadowed_uris: Vec<String>,
}

impl CatalogSnapshot {
    pub fn empty() -> Self {
        Self {
            version: 0,
            descriptors: BTreeMap::new(),
            routing_table: HashMap::new(),
            logical_winners: HashMap::new(),
            shadowed_uris: Vec::new(),
        }
    }

    /// Resolve a tool descriptor by exposed name (`script__foo`), URI (`script://foo`), or logical name (`foo`).
    pub fn resolve_descriptor(&self, query: &str) -> Option<&ToolDescriptor> {
        if let Some(desc) = self.descriptors.get(query) {
            return Some(desc);
        }
        if let Some(winner_name) = self.logical_winners.get(query) {
            if let Some(desc) = self.descriptors.get(winner_name) {
                return Some(desc);
            }
        }
        self.descriptors.values().find(|d| d.uri == query)
    }

    /// Resolve the owning [`ToolProvider`] and canonical exposed tool name.
    pub fn resolve_route(&self, query: &str) -> Option<(&ToolDescriptor, Arc<dyn ToolProvider>)> {
        let desc = self.resolve_descriptor(query)?;
        let provider = self.routing_table.get(&desc.name)?.clone();
        Some((desc, provider))
    }
}

/// Lock-free double-buffered tool registry backed by `ArcSwap<CatalogSnapshot>` (§4.1, §4.2, §6.2).
#[derive(Clone)]
pub struct ToolRegistry {
    snapshot: Arc<ArcSwap<CatalogSnapshot>>,
    version_seq: Arc<AtomicU64>,
    providers: Arc<RwLock<Vec<Arc<dyn ToolProvider>>>>,
    script_provider: Arc<ScriptToolProvider>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        let script_provider = ScriptToolProvider::shared("local-scripts");
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(CatalogSnapshot::empty())),
            version_seq: Arc::new(AtomicU64::new(0)),
            providers: Arc::new(RwLock::new(vec![
                Arc::clone(&script_provider) as Arc<dyn ToolProvider>
            ])),
            script_provider,
        }
    }

    /// Lock-free snapshot load for the current inference turn (§4.1, §6.2).
    pub fn current(&self) -> Arc<CatalogSnapshot> {
        self.snapshot.load_full()
    }

    /// Atomically commit a newly constructed `CatalogSnapshot` (§6.2).
    pub fn commit_update(&self, new_snapshot: CatalogSnapshot) {
        self.snapshot.store(Arc::new(new_snapshot));
    }

    pub fn script_provider(&self) -> &Arc<ScriptToolProvider> {
        &self.script_provider
    }

    /// Register a `ToolProvider` (such as `thunder-std` Core MCP or an external MCP server) and refresh the snapshot.
    pub async fn register_provider(
        &self,
        provider: Arc<dyn ToolProvider>,
    ) -> Result<Arc<CatalogSnapshot>, ToolExecutionError> {
        {
            let mut guard = self.providers.write();
            guard.retain(|p| p.provider_id() != provider.provider_id());
            guard.push(provider);
        }
        self.refresh().await
    }

    /// Rescan `.thunder/tools/` and atomically publish a new `CatalogSnapshot`.
    pub async fn reload_scripts_dir(
        &self,
        tools_dir: &Path,
    ) -> Result<Arc<CatalogSnapshot>, ToolExecutionError> {
        let entries = scan_scripts_dir(tools_dir)
            .await
            .map_err(ToolExecutionError::IOError)?;
        self.script_provider.replace_all(entries);
        self.refresh().await
    }

    /// Rebuild `CatalogSnapshot` from all active providers applying `Local Script > External MCP > Core` shadowing (§4.2).
    pub async fn refresh(&self) -> Result<Arc<CatalogSnapshot>, ToolExecutionError> {
        let providers = self.providers.read().clone();
        let mut candidates: Vec<(ToolDescriptor, Arc<dyn ToolProvider>)> = Vec::new();

        for provider in providers {
            match provider.list_tools().await {
                Ok(list) => {
                    for desc in list {
                        candidates.push((desc, Arc::clone(&provider)));
                    }
                }
                Err(err) => {
                    warn!(
                        provider_id = %provider.provider_id(),
                        error = %err,
                        "Failed to list tools from provider during catalog refresh"
                    );
                }
            }
        }

        // Sort candidates by descending priority (`Script(3) > Mcp(2) > Core(1)`), then by name.
        candidates.sort_by(|(a, _), (b, _)| {
            b.priority()
                .cmp(&a.priority())
                .then_with(|| a.name.cmp(&b.name))
        });

        let mut descriptors = BTreeMap::new();
        let mut routing_table = HashMap::new();
        let mut logical_winners: HashMap<String, (String, u8, String)> = HashMap::new();
        let mut shadowed_uris = Vec::new();

        for (desc, provider) in candidates {
            let logical = desc.logical_name();
            let prio = desc.priority();

            if let Some((winner_exposed, winner_prio, winner_uri)) = logical_winners.get(&logical) {
                if *winner_prio >= prio {
                    warn!(
                        shadowed_uri = %desc.uri,
                        shadowed_name = %desc.name,
                        winner_uri = %winner_uri,
                        winner_name = %winner_exposed,
                        "Tool shadowed by higher-priority namespace implementation"
                    );
                    shadowed_uris.push(desc.uri.clone());
                    continue;
                }
            }

            logical_winners.insert(logical.clone(), (desc.name.clone(), prio, desc.uri.clone()));
            routing_table.insert(desc.name.clone(), Arc::clone(&provider));
            routing_table.insert(desc.uri.clone(), Arc::clone(&provider));
            routing_table.insert(logical, Arc::clone(&provider));
            descriptors.insert(desc.name.clone(), desc);
        }

        let version = self.version_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let snapshot = CatalogSnapshot {
            version,
            descriptors,
            routing_table,
            logical_winners: logical_winners
                .into_iter()
                .map(|(k, (exposed, _, _))| (k, exposed))
                .collect(),
            shadowed_uris,
        };

        self.commit_update(snapshot);
        Ok(self.current())
    }

    /// Execute a tool against a frozen snapshot (`CatalogSnapshot`).
    pub async fn call_with_snapshot(
        snapshot: &CatalogSnapshot,
        name: &str,
        arguments: serde_json::Value,
        ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        let (desc, provider) = snapshot
            .resolve_route(name)
            .ok_or_else(|| ToolExecutionError::ToolNotFound(name.to_string()))?;
        provider.call_tool(&desc.name, arguments, ctx).await
    }
}
