//! `thunderctl status` — the read-only drift report.
//!
//! Three things are compared: what the desktop would spawn *next*, what is actually
//! running *now*, and what exists on disk (installed copy, repo builds, bundled copy).

use std::path::Path;
use std::time::SystemTime;

use crate::build::{self, Profile};
use crate::paths;
use crate::proc::{self, RunningDaemon};
use crate::resolve::{self, Kind, Resolution};
use crate::settings::DesktopSettings;

const PROFILES: [Profile; 3] = [Profile::Release, Profile::TuiDev, Profile::Dev];

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Human age of an epoch-millisecond timestamp relative to now.
pub fn age(ms: u64) -> String {
    if ms == 0 {
        return "unknown".to_string();
    }
    let now = epoch_ms();
    if ms >= now {
        return "刚刚".to_string();
    }
    let seconds = (now - ms) / 1000;
    if seconds < 60 {
        "刚刚".to_string()
    } else if seconds < 3600 {
        format!("{} 分钟前", seconds / 60)
    } else if seconds < 86_400 {
        format!("{} 小时前", seconds / 3600)
    } else {
        format!("{} 天前", seconds / 86_400)
    }
}

pub fn run(repo: Option<&Path>) -> Result<(), String> {
    let settings = DesktopSettings::load()?;
    let thunder = settings.thunder();

    let daemon = resolve::resolve(Kind::Daemon, settings.root.get("thunder"), repo);
    let tui = resolve::resolve(Kind::Tui, settings.root.get("thunder"), repo);
    let running = proc::running_daemons();

    print_data(&thunder);
    print_resolution("Desktop daemon 解析链", &daemon);
    print_resolution("Desktop TUI 解析链", &tui);
    print_running(&running);
    if let Some(repo) = repo {
        print_artifacts(repo);
    }
    print_install();
    print_warnings(&thunder, &daemon, &tui, &running, repo);
    Ok(())
}

fn print_data(thunder: &crate::settings::ThunderSettings) {
    let data_root = paths::data_root();
    println!("数据根目录");
    println!("  {:<20} {}", "data root", data_root.display());
    if let Ok(raw) = std::env::var(thunder_protocol::THUNDER_CONFIG_DIR_ENV) {
        if !raw.trim().is_empty() {
            println!("  {:<20} {} (来自环境变量)", "THUNDER_CONFIG_DIR", raw);
        }
    }
    let conversations = data_root.join("conversations");
    let (count, bytes) = conversation_stats(&conversations);
    println!(
        "  {:<20} {} 个会话 · {}",
        "conversations",
        count,
        human_size(bytes)
    );
    println!(
        "  {:<20} {}",
        "settings",
        paths::desktop_settings_file().display()
    );
    let overrides: Vec<String> = [
        ("repoPath", thunder.repo_path.as_deref()),
        ("daemonPath", thunder.daemon_path.as_deref()),
        ("tuiPath", thunder.tui_path.as_deref()),
    ]
    .iter()
    .filter_map(|(key, value)| value.map(|v| format!("{key}={v}")))
    .collect();
    if overrides.is_empty() {
        println!("  {:<20} （未设置 → 跟随 App 自带版本）", "thunder.*");
    } else {
        for line in overrides {
            println!("  {:<20} {}", "thunder.*", line);
        }
    }
    println!();
}

fn print_resolution(title: &str, resolution: &Resolution) {
    println!("{title}");
    if resolution.entries.is_empty() {
        println!("  （没有候选路径）");
    }
    for entry in &resolution.entries {
        let marker = if resolution
            .winner
            .as_ref()
            .is_some_and(|w| w.binary == entry.binary)
        {
            "✓"
        } else if entry.is_runnable() {
            "·"
        } else {
            "✗"
        };
        println!("  {marker} {:<38} {}", entry.source, entry.describe());
    }
    match &resolution.winner {
        Some(winner) => println!("  → 下次 spawn 会用: {}", winner.binary.display()),
        None => println!("  → 下次 spawn 会用: 找不到可用二进制（Desktop 会报错）"),
    }
    println!();
}

fn print_running(running: &[RunningDaemon]) {
    println!("运行中");
    if running.is_empty() {
        println!("  （没有 thunder-daemon 在跑）");
        println!();
        return;
    }
    for daemon in running {
        let mtime = proc::mtime_ms(&daemon.exe);
        let exe_text = daemon.exe.display().to_string();
        let kind = if daemon.is_script {
            "script"
        } else if daemon.exe.starts_with(paths::install_dir()) {
            "installed"
        } else if exe_text.contains(".app/Contents/Resources/") {
            "bundled"
        } else {
            "repo-build"
        };
        println!(
            "  pid {:<7} {:<11} {}",
            daemon.pid,
            kind,
            daemon.exe.display()
        );
        println!("  {:<21} 构建于 {}", "", age(mtime));
        if let Some(state) = daemon.read_state() {
            let reload = state
                .get("hot_reload")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            println!(
                "  {:<21} v{} · 协议 v{} · 热重启{}",
                "",
                state.get("version").and_then(|v| v.as_str()).unwrap_or("?"),
                state
                    .get("protocol_version")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                if reload { "支持" } else { "不支持" }
            );
        }
    }
    println!();
}

fn print_artifacts(repo: &Path) {
    println!("本仓库构建产物 ({})", repo.display());
    for profile in PROFILES {
        let dir = build::target_dir(repo).join(profile.dir_name());
        let daemon = dir.join("thunder-daemon");
        let daemon_line = if daemon.is_file() {
            format!(
                "{:<14} {}",
                human_size(proc::file_size(&daemon)),
                age(proc::mtime_ms(&daemon))
            )
        } else {
            "缺失".to_string()
        };
        let tui = dir.join("thunder-tui");
        let tui_line = if tui.is_file() {
            format!(
                "{:<14} {}",
                human_size(proc::file_size(&tui)),
                age(proc::mtime_ms(&tui))
            )
        } else {
            "缺失".to_string()
        };
        println!(
            "  {:<12} daemon {:<32} tui {}",
            profile.dir_name(),
            daemon_line,
            tui_line
        );
    }
    println!();
}

fn print_install() {
    let install = paths::install_dir();
    println!("安装位 ({})", install.display());
    let mut any = false;
    for name in paths::ALL_BINARIES {
        let path = install.join(name);
        if path.is_file() {
            any = true;
            println!(
                "  {:<14} {:<14} {}",
                name,
                human_size(proc::file_size(&path)),
                age(proc::mtime_ms(&path))
            );
        }
    }
    if !any {
        println!("  （空 — 还没有执行过 thunderctl install）");
    }
    println!();
}

fn print_warnings(
    thunder: &crate::settings::ThunderSettings,
    daemon: &Resolution,
    tui: &Resolution,
    running: &[RunningDaemon],
    repo: Option<&Path>,
) {
    let mut warnings: Vec<String> = Vec::new();

    for (label, value, resolution) in [
        ("daemonPath", thunder.daemon_path.as_deref(), daemon),
        ("tuiPath", thunder.tui_path.as_deref(), tui),
    ] {
        if let Some(path) = value {
            if !std::path::Path::new(path).is_file() {
                warnings.push(format!(
                    "settings.thunder.{label} 指向的文件不存在（{path}）→ Desktop 会退回下一优先级"
                ));
            }
            if !resolution.winner.as_ref().is_some_and(|w| w.exists) {
                warnings.push(format!(
                    "settings.thunder.{label} 生效了，但整条解析链都找不到可执行文件"
                ));
            }
        }
    }

    let bundled_mtime = daemon
        .entries
        .iter()
        .find(|e| e.source.starts_with("bundled"))
        .map(|e| proc::mtime_ms(&e.binary))
        .unwrap_or(0);
    let installed_daemon = paths::install_dir().join("thunder-daemon");
    if thunder.daemon_path.is_some()
        && bundled_mtime > 0
        && proc::mtime_ms(&installed_daemon) > 0
        && bundled_mtime > proc::mtime_ms(&installed_daemon) + 60_000
    {
        warnings.push(format!(
            "回退风险：App 自带 daemon（{}）比你的安装版新 {}，但 daemonPath 让它被忽略；`thunderctl bundled` 可交还给 App",
            age(bundled_mtime),
            age(proc::mtime_ms(&installed_daemon))
        ));
    }

    if let Some(winner) = daemon.winner.as_ref().filter(|w| w.exists) {
        let winner_mtime = proc::mtime_ms(&winner.binary);
        for process in running {
            if process.exe != winner.binary {
                warnings.push(format!(
                    "运行中的 daemon 和 Desktop 下次会选的不同（跑着 {}，下次 {}）→ 重启 App 或 `thunderctl reload`",
                    process.exe.display(),
                    winner.binary.display()
                ));
                continue;
            }
            if winner_mtime > proc::mtime_ms(&process.exe) + 1000 {
                warnings.push(
                    "磁盘上的 daemon 比运行中的新 → `thunderctl reload` 即可热换二进制（不改 pid）"
                        .to_string(),
                );
            }
        }
        if running.is_empty() {
            warnings.push(format!(
                "Desktop 会 spawn {}，但当前没有常驻 daemon（下次使用 Thunder 会话时自动拉起）",
                winner.binary.display()
            ));
        }
        if winner.binary.starts_with(paths::install_dir()) && repo.is_none() {
            warnings.push("当前目录不在 thunder 仓库内，无法核对仓库构建产物".to_string());
        }
    }

    let installed = paths::install_dir().join("thunder-daemon");
    if !installed.is_file() {
        warnings.push(
            "还没有安装位二进制 → `thunderctl install` 才会创建 ~/.cargo/bin/thunder-*".to_string(),
        );
    }

    println!("提示");
    if warnings.is_empty() {
        println!("  ✓ 无异常");
    } else {
        for warning in warnings {
            println!("  ⚠ {warning}");
        }
    }
}

/// The conversation store keeps one directory per session, so sessions are the
/// immediate subdirectories and the byte count has to walk inside them.
fn conversation_stats(dir: &Path) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut sessions = 0;
    let mut bytes = 0;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            sessions += 1;
            bytes += dir_size(&entry.path(), 4);
        } else if file_type.is_file() {
            bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        }
    }
    (sessions, bytes)
}

fn dir_size(dir: &Path, depth_left: usize) -> u64 {
    if depth_left == 0 {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut bytes = 0;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            bytes += dir_size(&entry.path(), depth_left - 1);
        } else if file_type.is_file() {
            bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_ages_read_naturally() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(6 * 1024 * 1024), "6.0 MB");
        assert_eq!(age(0), "unknown");
        assert_eq!(age(epoch_ms()), "刚刚");
        assert_eq!(age(epoch_ms().saturating_sub(120_000)), "2 分钟前");
        assert_eq!(age(epoch_ms().saturating_sub(3 * 3_600_000)), "3 小时前");
        assert_eq!(age(epoch_ms().saturating_sub(2 * 86_400_000)), "2 天前");
    }
}
