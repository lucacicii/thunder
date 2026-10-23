use crate::capture::OcrEngine;
use crate::composer_sync::ComposerSyncEngine;
use crate::config::MemoryConfig;
use crate::db::Database;
use crate::error::MemoryError;
use crate::synthesis::SynthesisEngine;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

pub struct MemoryDaemon {
    config: MemoryConfig,
    db: Database,
    ocr: Arc<OcrEngine>,
    synthesis: Arc<SynthesisEngine>,
    composer_sync: Arc<ComposerSyncEngine>,
}

impl MemoryDaemon {
    pub fn new(config: MemoryConfig) -> Result<Self, MemoryError> {
        config.ensure_dirs()?;
        let db = Database::open(&config.db_path)?;
        let ocr = Arc::new(OcrEngine::new()?);
        let synthesis = Arc::new(SynthesisEngine::new(db.clone(), config.clone()));
        let composer_sync = Arc::new(ComposerSyncEngine::new(db.clone(), config.clone()));

        Ok(Self {
            config,
            db,
            ocr,
            synthesis,
            composer_sync,
        })
    }

    /// Run foreground daemon event loop until cancelled
    pub async fn run_foreground(&self, cancel: CancellationToken) -> Result<(), MemoryError> {
        info!("Starting thunder-memory daemon in foreground...");
        info!("Database: {:?}", self.config.db_path);
        info!(
            "Dynamic perception heartbeat: base={}s, min={}s, max={}s",
            self.config.capture_interval_secs,
            self.config.min_capture_interval_secs,
            self.config.max_capture_interval_secs
        );
        info!("Synthesis interval: {}s", self.config.synthesis_interval_secs);

        let mut current_interval = Duration::from_secs(self.config.capture_interval_secs);
        let min_interval = Duration::from_secs(self.config.min_capture_interval_secs.max(1));
        let max_interval = Duration::from_secs(
            self.config
                .max_capture_interval_secs
                .max(self.config.capture_interval_secs),
        );
        let mut consecutive_idle_ticks: u32 = 0;

        let sleep = tokio::time::sleep(current_interval);
        tokio::pin!(sleep);

        let mut synthesis_interval =
            tokio::time::interval(Duration::from_secs(self.config.synthesis_interval_secs));
        let mut sync_interval = tokio::time::interval(Duration::from_secs(600)); // 10 min
        let mut maintenance_interval = tokio::time::interval(Duration::from_secs(86_400)); // 24h maintenance

        let mut last_hash = self.db.get_last_capture_hash()?;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("Received shutdown signal, stopping thunder-memory daemon...");
                    break;
                }
                () = &mut sleep => {
                    match self.ocr.capture_screen() {
                        Ok(rec) => {
                            // 1. Noise Suppression Check
                            match self.db.is_window_suppressed(&rec.bundle_id, &rec.window_title) {
                                Ok(true) => {
                                    debug!(
                                        "Screen suppressed by mask: app='{}', bundle='{}', win='{}'",
                                        rec.app_name, rec.bundle_id, rec.window_title
                                    );
                                    // Decelerate during background/suppressed windows
                                    consecutive_idle_ticks = consecutive_idle_ticks.saturating_add(1);
                                    let next_secs = (current_interval.as_secs() * 2).min(max_interval.as_secs());
                                    current_interval = Duration::from_secs(next_secs);
                                }
                                _ => {
                                    if rec.avg_confidence >= self.config.min_confidence {
                                        if OcrEngine::is_diff(last_hash.as_deref(), &rec.content_hash) {
                                            info!(
                                                "Screen changed: app='{}', win='{}', chars={}",
                                                rec.app_name, rec.window_title, rec.full_text.len()
                                            );
                                            if let Err(e) = self.db.insert_capture(&rec) {
                                                error!("Failed to store screen capture: {}", e);
                                            } else {
                                                last_hash = Some(rec.content_hash);
                                            }
                                            // Dynamic acceleration: snap to min_interval upon active UI change
                                            consecutive_idle_ticks = 0;
                                            current_interval = min_interval;
                                        } else {
                                            // Screen unchanged: progressive deceleration
                                            consecutive_idle_ticks = consecutive_idle_ticks.saturating_add(1);
                                            if consecutive_idle_ticks >= 2 {
                                                let next_secs = (current_interval.as_secs() + 5).min(max_interval.as_secs());
                                                current_interval = Duration::from_secs(next_secs);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Screen capture/OCR tick failed: {}", e);
                            current_interval = Duration::from_secs(self.config.capture_interval_secs);
                        }
                    }
                    sleep.as_mut().reset(tokio::time::Instant::now() + current_interval);
                }
                _ = synthesis_interval.tick() => {
                    info!("Running periodic memory synthesis...");
                    match self.synthesis.run_synthesis_cycle().await {
                        Ok(count) => {
                            if count > 0 {
                                info!("Consolidated {} memory episodes into long-term memory", count);
                            }
                        }
                        Err(e) => {
                            error!("Synthesis cycle failed: {}", e);
                        }
                    }
                }
                _ = sync_interval.tick() => {
                    info!("Checking for new composer sends to sync...");
                    match self.composer_sync.sync_composer_sends() {
                        Ok(count) => {
                            if count > 0 {
                                info!("Synced {} composer sends", count);
                            }
                        }
                        Err(e) => {
                            warn!("Composer sync tick failed: {}", e);
                        }
                    }
                }
                _ = maintenance_interval.tick() => {
                    info!("Running scheduled aggressive compaction and cold archiving...");
                    match self.db.compact_and_archive(
                        &self.config.archive_db_path,
                        self.config.raw_capture_retention_days,
                        self.config.memory_archive_days,
                    ) {
                        Ok(res) => {
                            info!(
                                "Compaction complete: pruned {} captures, archived {} memories",
                                res.pruned_captures, res.archived_memories
                            );
                        }
                        Err(e) => {
                            warn!("Scheduled compaction failed: {}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn pid_file_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".thunder").join("thunder-memory.pid")
    }

    pub fn log_file_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".thunder")
            .join("logs")
            .join("thunder-memory.log")
    }

    /// Start daemon in background detached process
    pub fn start_background() -> Result<u32, MemoryError> {
        let pid_path = Self::pid_file_path();
        if let Some(pid) = Self::read_pid(&pid_path) {
            if Self::is_pid_running(pid) {
                return Err(MemoryError::Other(format!(
                    "thunder-memory daemon is already running (PID {})",
                    pid
                )));
            }
        }

        let log_path = Self::log_file_path();
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let current_exe = std::env::current_exe()?;
        let log_file = File::create(&log_path)?;
        let err_file = log_file.try_clone()?;

        let child = Command::new(&current_exe)
            .arg("run")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(err_file))
            .spawn()
            .map_err(|e| MemoryError::Other(format!("Failed to spawn daemon: {}", e)))?;

        let pid = child.id();
        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&pid_path, pid.to_string())?;

        Ok(pid)
    }

    /// Stop background daemon
    pub fn stop_background() -> Result<bool, MemoryError> {
        let pid_path = Self::pid_file_path();
        if let Some(pid) = Self::read_pid(&pid_path) {
            if Self::is_pid_running(pid) {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                // Wait up to 5 seconds for termination
                for _ in 0..50 {
                    std::thread::sleep(Duration::from_millis(100));
                    if !Self::is_pid_running(pid) {
                        break;
                    }
                }
                if Self::is_pid_running(pid) {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                }
            }
            let _ = std::fs::remove_file(&pid_path);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Check if background daemon is running
    pub fn is_daemon_running() -> Option<u32> {
        let pid_path = Self::pid_file_path();
        Self::read_pid(&pid_path).filter(|&pid| Self::is_pid_running(pid))
    }

    fn read_pid(path: &Path) -> Option<u32> {
        if path.exists() {
            std::fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()
        } else {
            None
        }
    }

    fn is_pid_running(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    /// Generate macOS LaunchAgent plist content
    pub fn generate_launchagent_plist(exe_path: &Path, log_path: &Path) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.thunder.memory</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
</dict>
</plist>"#,
            exe_path.display(),
            log_path.display(),
            log_path.display()
        )
    }

    /// Install LaunchAgent to ~/Library/LaunchAgents
    pub fn install_launchagent() -> Result<PathBuf, MemoryError> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let agents_dir = PathBuf::from(home).join("Library").join("LaunchAgents");
        std::fs::create_dir_all(&agents_dir)?;

        let plist_path = agents_dir.join("com.thunder.memory.plist");
        let current_exe = std::env::current_exe()?;
        let log_path = Self::log_file_path();

        let plist_content = Self::generate_launchagent_plist(&current_exe, &log_path);
        std::fs::write(&plist_path, plist_content)?;

        // Run launchctl load
        let _ = Command::new("launchctl")
            .arg("load")
            .arg("-w")
            .arg(&plist_path)
            .status();

        Ok(plist_path)
    }

    /// Uninstall LaunchAgent
    pub fn uninstall_launchagent() -> Result<bool, MemoryError> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let plist_path = PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join("com.thunder.memory.plist");

        if plist_path.exists() {
            let _ = Command::new("launchctl")
                .arg("unload")
                .arg(&plist_path)
                .status();
            std::fs::remove_file(&plist_path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
