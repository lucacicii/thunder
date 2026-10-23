use std::path::PathBuf;
use thunder_memory::capture::OcrEngine;
use thunder_memory::composer_sync::ComposerSyncEngine;
use thunder_memory::config::MemoryConfig;
use thunder_memory::daemon::MemoryDaemon;
use thunder_memory::db::Database;
use thunder_memory::search::MemorySearchEngine;
use thunder_memory::synthesis::SynthesisEngine;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str()).unwrap_or("status");

    // Initialize tracing to stderr so stdout is strictly clean for MCP JSON-RPC
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();

    let config = MemoryConfig::load();

    match subcommand {
        "run" => {
            let daemon = MemoryDaemon::new(config)?;
            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();

            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    cancel_clone.cancel();
                }
            });

            daemon.run_foreground(cancel).await?;
        }

        "start" => {
            match MemoryDaemon::start_background() {
                Ok(pid) => {
                    println!("🚀 thunder-memory daemon started in background (PID: {})", pid);
                    println!("   Log file: {:?}", MemoryDaemon::log_file_path());
                    println!("   PID file: {:?}", MemoryDaemon::pid_file_path());
                }
                Err(e) => {
                    eprintln!("❌ Failed to start background daemon: {}", e);
                    std::process::exit(1);
                }
            }
        }

        "stop" => {
            match MemoryDaemon::stop_background() {
                Ok(true) => {
                    println!("🛑 thunder-memory daemon stopped successfully.");
                }
                Ok(false) => {
                    println!("ℹ️  thunder-memory daemon was not running.");
                }
                Err(e) => {
                    eprintln!("❌ Failed to stop daemon: {}", e);
                    std::process::exit(1);
                }
            }
        }

        "status" => {
            println!("══════════════════════════════════════════════════════════════════");
            println!("  🧠 Thunder Memory - Daemon & Storage Status");
            println!("══════════════════════════════════════════════════════════════════");

            if let Some(pid) = MemoryDaemon::is_daemon_running() {
                println!("  ● Daemon Status: RUNNING (PID: {})", pid);
            } else {
                println!("  ○ Daemon Status: STOPPED");
            }

            println!("  📂 Database:     {:?}", config.db_path);
            println!("  📋 Composer DB:  {:?}", config.resolve_composer_db_path().unwrap_or_else(|| PathBuf::from("not found")));
            println!("  ⏱️  Perception:   Base={}s, Min={}s, Max={}s | Synthesis={}s", config.capture_interval_secs, config.min_capture_interval_secs, config.max_capture_interval_secs, config.synthesis_interval_secs);
            // Thunder-memory infers nothing itself: the synthesis tier is the
            // configured OpenAI-compatible endpoint, or the rule extractor when
            // there is none. So a stopped Ollama is worth saying out loud —
            // otherwise synthesis quietly degrades to heuristics.
            let edge_info = match &config.llm_endpoint {
                Some(endpoint) => format!(
                    "{} ({}) — {}",
                    config.llm_model,
                    endpoint,
                    probe_llm_endpoint(endpoint, &config.llm_model).await
                ),
                None => "No endpoint configured — Heuristic Rule Engine (Offline / Zero-Cost)".to_string(),
            };
            println!("  🔬 Synthesis Model:    {}", edge_info);

            if config.db_path.exists() {
                let db = Database::open(&config.db_path)?;
                let stats = db.get_stats()?;
                println!("──────────────────────────────────────────────────────────────────");
                println!("  📊 Total Screen Captures:      {}", stats.total_captures);
                println!("  ⏳ Unprocessed Captures:       {}", stats.unprocessed_captures);
                println!("  🌟 Long-Term Memories:         {}", stats.long_term_memories);
                println!("  🤖 Agent Rules / Guidelines:   {}", stats.agent_interactions);
                println!("  🎓 Composer Training Samples:  {}", stats.training_samples);
                println!("  🛡️  Noise Suppression Hits:     {}", stats.suppression_mask_hits);
                if let Some(ms) = stats.last_capture_ms {
                    let secs_ago = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64 - ms) / 1000;
                    println!("  🕒 Last Screen Capture:        {}s ago", secs_ago.max(0));
                }
            } else {
                println!("  ℹ️  Database has not been created yet. Run `thunder-memory run` to start.");
            }
            println!("══════════════════════════════════════════════════════════════════");
        }

        "ocr-once" => {
            let save_to_db = args.iter().any(|a| a == "--save");
            println!("📸 Capturing screen and running Apple Vision OCR (save={})...", save_to_db);
            let ocr = OcrEngine::new()?;
            let rec = ocr.capture_screen()?;
            println!("──────────────────────────────────────────────────────────────────");
            println!("App:        {}", rec.app_name);
            println!("Bundle ID:  {}", rec.bundle_id);
            println!("Window:     {}", rec.window_title);
            println!("Confidence: {:.2}", rec.avg_confidence);
            println!("Hash:       {}", rec.content_hash);
            println!("Characters: {}", rec.full_text.len());
            println!("──────────────────────────────────────────────────────────────────");
            println!("Sample Text:\n{}", if rec.full_text.len() > 500 {
                format!("{}...", thunder_memory::safe_truncate_str(&rec.full_text, 500))
            } else {
                rec.full_text.clone()
            });

            if save_to_db {
                config.ensure_dirs()?;
                let db = Database::open(&config.db_path)?;
                let id = db.insert_capture(&rec)?;
                println!("💾 Saved screen capture to SQLite with id={}", id);
            }
        }

        "synthesize" => {
            println!("🧠 Running manual memory synthesis cycle...");
            config.ensure_dirs()?;
            let db = Database::open(&config.db_path)?;
            let synthesis = SynthesisEngine::new(db, config);
            let count = synthesis.run_synthesis_cycle().await?;
            println!("✅ Synthesis cycle complete. Processed {} episodes.", count);
        }

        "sync-composer" => {
            println!("📥 Syncing workbench_composer_sends from desktop database...");
            config.ensure_dirs()?;
            let db = Database::open(&config.db_path)?;
            let syncer = ComposerSyncEngine::new(db.clone(), config);
            let count = syncer.sync_composer_sends()?;
            let stats = db.get_stats()?;
            println!("✅ Synced {} new sends. Total training samples: {}.", count, stats.training_samples);
        }

        "export-training" => {
            let format = args.get(2).map(|s| s.as_str()).unwrap_or("openai");
            let output_path = args.get(3).map(PathBuf::from).unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".thunder").join("training_dataset.jsonl")
            });

            config.ensure_dirs()?;
            let db = Database::open(&config.db_path)?;
            let syncer = ComposerSyncEngine::new(db, config);
            let count = syncer.export_dataset(&output_path, format, 0.4, None)?;
            println!("✅ Exported {} training records to {:?}", count, output_path);
        }

        "query" => {
            let query_text = args.get(2).map(|s| s.as_str()).unwrap_or("");
            if query_text.is_empty() {
                println!("Usage: thunder-memory query <search terms>");
                return Ok(());
            }

            let db = Database::open(&config.db_path)?;
            let agent_records = db.search_agent_interactions(query_text, 10)?;
            let search = MemorySearchEngine::new(db);
            let results = search.query(query_text, 10)?;

            println!("══════════════════════════════════════════════════════════════════");
            println!("  🔍 Memory Search Results for: '{}'", query_text);
            println!("══════════════════════════════════════════════════════════════════");

            if !agent_records.is_empty() {
                println!("  🤖 Agent Cognitive Records & Guidelines (Found {}):", agent_records.len());
                for (idx, r) in agent_records.iter().enumerate() {
                    let scope_badge = if r.scope == "global" {
                        "[GLOBAL]".to_string()
                    } else {
                        format!("[PROJECT: {}]", r.project_path.as_deref().unwrap_or("current"))
                    };
                    println!(
                        "  [{}] {} [{}] {} ({})",
                        idx + 1,
                        scope_badge,
                        r.category,
                        r.title,
                        r.status
                    );
                    println!("      {}", r.content);
                    if !r.entities.is_empty() {
                        println!("      Entities: {:?}", r.entities);
                    }
                }
                println!("──────────────────────────────────────────────────────────────────");
            }

            if !results.is_empty() {
                println!("  🖥️ Desktop & Screen History Insights (Found {}):", results.len());
                for (idx, item) in results.iter().enumerate() {
                    let m = &item.memory;
                    println!(
                        "  [{}] Score: {:.2} (Rel: {:.2}, Imp: {:.2}, Rec: {:.2}) | [{}] {}",
                        idx + 1,
                        item.total_score,
                        item.relevance_score,
                        item.importance_score_norm,
                        item.recency_score,
                        m.category,
                        m.title
                    );
                    println!("      Summary:  {}", m.summary);
                    println!("      Entities: {:?}", m.key_entities);
                }
                println!("──────────────────────────────────────────────────────────────────");
            }

            if agent_records.is_empty() && results.is_empty() {
                println!("  No matching memories or guidelines found.");
            }
        }

        "preferences" => {
            let project_path = args.get(2).map(|s| s.as_str());
            let db = Database::open(&config.db_path)?;
            let prefs = db.get_active_preferences(project_path)?;
            println!("══════════════════════════════════════════════════════════════════");
            println!("  🧠 Thunder Memory - Active Preferences & Constraints");
            if let Some(p) = project_path {
                println!("  Target Project: {}", p);
            } else {
                println!("  Target: Global only");
            }
            println!("══════════════════════════════════════════════════════════════════");

            if prefs.is_empty() {
                println!("  No active preferences or constraints recorded.");
            } else {
                for (idx, p) in prefs.iter().enumerate() {
                    let scope_badge = if p.scope == "global" {
                        "[GLOBAL]".to_string()
                    } else {
                        format!("[PROJECT: {}]", p.project_path.as_deref().unwrap_or("current"))
                    };
                    println!(
                        "  [{}] {} [{}] {} (by {})",
                        idx + 1,
                        scope_badge,
                        p.category,
                        p.title,
                        p.agent_name
                    );
                    println!("      {}", p.content);
                    if !p.entities.is_empty() {
                        println!("      Entities: {:?}", p.entities);
                    }
                    println!("──────────────────────────────────────────────────────────────────");
                }
            }
        }

        "record" => {
            if args.len() < 5 {
                println!("Usage: thunder-memory record <category> <title> <content> [--scope global|project] [--project <path>] [--agent <name>]");
                println!("Categories: constraint, user_preference, architecture_decision, bug_solution, task_milestone");
                return Ok(());
            }

            let category = args[2].clone();
            let title = args[3].clone();
            let content = args[4].clone();

            let mut scope = "global".to_string();
            let mut project_path = None;
            let mut agent_name = "cli".to_string();

            let mut idx = 5;
            while idx < args.len() {
                match args[idx].as_str() {
                    "--scope" => {
                        if let Some(val) = args.get(idx + 1) {
                            scope = val.clone();
                            idx += 1;
                        }
                    }
                    "--project" => {
                        if let Some(val) = args.get(idx + 1) {
                            project_path = Some(val.clone());
                            scope = "project".to_string();
                            idx += 1;
                        }
                    }
                    "--agent" => {
                        if let Some(val) = args.get(idx + 1) {
                            agent_name = val.clone();
                            idx += 1;
                        }
                    }
                    _ => {}
                }
                idx += 1;
            }

            config.ensure_dirs()?;
            let db = Database::open(&config.db_path)?;
            let new_item = thunder_memory::db::NewAgentInteraction {
                agent_name,
                scope,
                project_path,
                category,
                title: title.clone(),
                content,
                entities: vec![],
                suggested_importance: 8.0,
                metadata: None,
            };

            let id = db.insert_agent_interaction(&new_item)?;
            println!("✅ Successfully recorded memory rule #{} ('{}')", id, title);
        }

        "list-memories" => {
            let limit = args.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(10);
            let db = Database::open(&config.db_path)?;
            let memories = db.list_recent_memories(limit)?;

            println!("══════════════════════════════════════════════════════════════════");
            println!("  🌟 Recent Long-Term Memories ({})", memories.len());
            println!("══════════════════════════════════════════════════════════════════");

            for (idx, m) in memories.iter().enumerate() {
                println!(
                    "[{}] [{}] {} (Importance: {:.1})",
                    idx + 1,
                    m.category,
                    m.title,
                    m.importance_score
                );
                println!("    {}", m.summary);
                println!("    Entities: {:?}", m.key_entities);
                println!("──────────────────────────────────────────────────────────────────");
            }
        }

        "install-launchagent" => {
            match MemoryDaemon::install_launchagent() {
                Ok(path) => {
                    println!("✅ Successfully installed and loaded LaunchAgent at {:?}", path);
                    println!("   thunder-memory will now automatically start on login and run 24/7.");
                }
                Err(e) => {
                    eprintln!("❌ Failed to install LaunchAgent: {}", e);
                }
            }
        }

        "uninstall-launchagent" => {
            match MemoryDaemon::uninstall_launchagent() {
                Ok(true) => {
                    println!("✅ Successfully unloaded and removed LaunchAgent.");
                }
                Ok(false) => {
                    println!("ℹ️  LaunchAgent plist was not found.");
                }
                Err(e) => {
                    eprintln!("❌ Failed to uninstall LaunchAgent: {}", e);
                }
            }
        }

        "masks" => {
            let db = Database::open(&config.db_path)?;
            let masks = db.get_active_suppression_masks()?;
            println!("══════════════════════════════════════════════════════════════════");
            println!("  🛡️  Active Cognitive Noise Suppression Masks ({})", masks.len());
            println!("══════════════════════════════════════════════════════════════════");
            for m in masks {
                println!("  ● Bundle:   {}", m.bundle_id);
                println!("    Pattern:  {}", m.window_title_pattern);
                println!("    Reason:   {}", m.reason);
                println!("    Hits:     {}", m.hit_count);
                println!("──────────────────────────────────────────────────────────────────");
            }
        }

        "mask" => {
            if args.len() < 4 || args[1] != "add" {
                println!("Usage: thunder-memory mask add <bundle_id> <window_title_pattern> <reason>");
                return Ok(());
            }
            let db = Database::open(&config.db_path)?;
            let bundle = &args[2];
            let pattern = &args[3];
            let reason = if args.len() > 4 {
                args[4..].join(" ")
            } else {
                "User defined noise mask".to_string()
            };
            db.add_suppression_mask(bundle, pattern, &reason)?;
            println!("✅ Added suppression mask: bundle='{}', pattern='{}'", bundle, pattern);
        }

        "compact" => {
            let mut capture_days = config.raw_capture_retention_days;
            let mut archive_days = config.memory_archive_days;

            let mut i = 2;
            while i < args.len() {
                if args[i] == "--days" && i + 1 < args.len() {
                    capture_days = args[i + 1].parse().unwrap_or(capture_days);
                    i += 2;
                } else if args[i] == "--archive-days" && i + 1 < args.len() {
                    archive_days = args[i + 1].parse().unwrap_or(archive_days);
                    i += 2;
                } else {
                    i += 1;
                }
            }

            println!("🧹 Running aggressive compaction and cold archiving...");
            println!("   Ephemeral L1 retention: {} days", capture_days);
            println!("   Episodic L2 archive threshold: {} days", archive_days);
            println!("   Archive destination: {:?}", config.archive_db_path);

            let db = Database::open(&config.db_path)?;
            let res = db.compact_and_archive(&config.archive_db_path, capture_days, archive_days)?;

            println!("✅ Compaction complete!");
            println!("   - Pruned L1 captures:        {}", res.pruned_captures);
            println!("   - Archived L2 memories:      {}", res.archived_memories);
            if res.active_db_bytes_before > 0 {
                let kb_before = res.active_db_bytes_before as f64 / 1024.0;
                let kb_after = res.active_db_bytes_after as f64 / 1024.0;
                println!("   - Active DB size:            {:.1} KB -> {:.1} KB", kb_before, kb_after);
            }
        }

        "mcp" => {
            config.ensure_dirs()?;
            let db = Database::open(&config.db_path)?;
            thunder_memory::run_mcp_stdio(db).await?;
        }

        _ => {
            println!("Thunder Memory - Autonomous Desktop Perception & Memory Engine");
            println!("\nUsage: thunder-memory <command> [options]");
            println!("\nCommands:");
            println!("  run                     Run daemon in foreground with live logs");
            println!("  start                   Start daemon in background detached");
            println!("  stop                    Stop running background daemon");
            println!("  status                  Show daemon and database status");
            println!("  ocr-once                Run instant screen capture & OCR test");
            println!("  synthesize              Trigger manual memory synthesis cycle");
            println!("  sync-composer           Sync workbench_composer_sends from desktop.db");
            println!("  export-training [fmt]   Export training dataset (openai | alpaca | sharegpt)");
            println!("  query <search terms>    Query memory with Generative Agents scoring");
            println!("  preferences [project]   Show active user preferences & constraints");
            println!("  record <cat> <title>    Record explicit memory/preference rule");
            println!("  compact                 Run aggressive compaction and cold archiving");
            println!("  masks                   List active noise suppression masks and hit statistics");
            println!("  mask add <bundle> <pat> Add an autonomous cognitive noise suppression mask");
            println!("  list-memories [limit]   List recent long-term memories");
            println!("  mcp                     Run standard JSON-RPC 2.0 MCP server over stdio");
            println!("  install-launchagent     Install macOS LaunchAgent for autostart");
            println!("  uninstall-launchagent   Remove macOS LaunchAgent");
        }
    }

    Ok(())
}

/// Ask an OpenAI-compatible endpoint what it serves, so `status` can say whether
/// the synthesis model is actually there.
///
/// A stopped Ollama is invisible otherwise: synthesis logs one warning per cycle
/// and quietly falls back to the rule extractor, which looks like "the model is
/// just not very good" rather than "nothing is answering".
async fn probe_llm_endpoint(endpoint: &str, model: &str) -> String {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1500))
        .build()
    else {
        return "probe failed".to_string();
    };

    let resp = match client.get(&url).send().await {
        Ok(resp) => resp,
        Err(e) => return format!("unreachable ({e})"),
    };
    if !resp.status().is_success() {
        return format!("HTTP {} from /models", resp.status());
    }

    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let served: Vec<String> = body["data"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // Ollama lists names with their tag (`qwen2.5-coder:0.5b`); a config that
    // names only the family still resolves to the tag Ollama would default to.
    let wanted = model.trim();
    if served
        .iter()
        .any(|id| id == wanted || id.split(':').next() == Some(wanted))
    {
        "reachable".to_string()
    } else if served.is_empty() {
        "endpoint up, serves no models".to_string()
    } else {
        format!("endpoint up but '{wanted}' is not pulled (has: {})", served.join(", "))
    }
}
