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

    let config = MemoryConfig::default();

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
            println!("  ⏱️  Intervals:    Capture={}s, Synthesis={}s", config.capture_interval_secs, config.synthesis_interval_secs);

            if config.db_path.exists() {
                let db = Database::open(&config.db_path)?;
                let stats = db.get_stats()?;
                println!("──────────────────────────────────────────────────────────────────");
                println!("  📊 Total Screen Captures:      {}", stats.total_captures);
                println!("  ⏳ Unprocessed Captures:       {}", stats.unprocessed_captures);
                println!("  🌟 Long-Term Memories:         {}", stats.long_term_memories);
                println!("  🎓 Composer Training Samples:  {}", stats.training_samples);
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
                format!("{}...", &rec.full_text[..500])
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
            let search = MemorySearchEngine::new(db);
            let results = search.query(query_text, 10)?;

            println!("══════════════════════════════════════════════════════════════════");
            println!("  🔍 Memory Search Results for: '{}' (Found {})", query_text, results.len());
            println!("══════════════════════════════════════════════════════════════════");

            for (idx, item) in results.iter().enumerate() {
                let m = &item.memory;
                println!(
                    "[{}] Score: {:.2} (Rel: {:.2}, Imp: {:.2}, Rec: {:.2}) | [{}] {}",
                    idx + 1,
                    item.total_score,
                    item.relevance_score,
                    item.importance_score_norm,
                    item.recency_score,
                    m.category,
                    m.title
                );
                println!("    Summary:  {}", m.summary);
                println!("    Entities: {:?}", m.key_entities);
                println!("──────────────────────────────────────────────────────────────────");
            }
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
            println!("  list-memories [limit]   List recent long-term memories");
            println!("  mcp                     Run standard JSON-RPC 2.0 MCP server over stdio");
            println!("  install-launchagent     Install macOS LaunchAgent for autostart");
            println!("  uninstall-launchagent   Remove macOS LaunchAgent");
        }
    }

    Ok(())
}
