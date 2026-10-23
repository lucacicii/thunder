//! `thunderctl` — the one-command wrapper around Thunder's three runtime states.
//!
//! * `dev`     → the desktop (and `run.sh`) spawn this checkout's `target/<profile>/`
//! * `prod`    → they spawn the installed copy in `~/.cargo/bin`
//! * `bundled` → hand control back to the sidecar shipped inside Agent Resume
//!
//! `status` explains which of the three is in effect, `reload` swaps the running
//! daemon's binary in place (same pid, no app restart).

mod build;
mod paths;
mod proc;
mod report;
mod resolve;
mod settings;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use build::Profile;
use settings::{DesktopSettings, ThunderSettings};

const USAGE: &str = "\
thunderctl — Thunder 运行时三态管理

用法:
  thunderctl status                    打印解析链 / 运行中进程 / 产物与漂移提示
  thunderctl install [--profile P]     构建并把 4 个二进制固化到 ~/.cargo/bin
  thunderctl dev     [--profile P]     构建 + 把 Desktop 指向本仓库 target/<profile>
  thunderctl prod                      把 Desktop 指向 ~/.cargo/bin（安装版）
  thunderctl bundled                   清空 thunder.* 配置，交还给 App 自带版本
  thunderctl reload                    让常驻 daemon 原地 exec 新二进制（pid 不变）

选项:
  --profile P    release | tui-dev | dev   （dev 默认 tui-dev，install 默认 release）
  --repo PATH    显式指定 thunder 仓库根目录（也读 THUNDER_PATH）
  --no-build     跳过 cargo build，只切换配置
  --dry-run      只打印将要写入的配置，不落盘
  -h, --help     帮助
";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("thunderctl: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        print!("{USAGE}");
        return Ok(());
    }
    if args
        .iter()
        .any(|a| a == "-h" || a == "--help" || a == "help")
    {
        print!("{USAGE}");
        return Ok(());
    }

    let repo_flag = take_value(&mut args, "--repo").or_else(|| take_value(&mut args, "-r"));
    let dry_run = take_flag(&mut args, "--dry-run");
    let no_build = take_flag(&mut args, "--no-build");
    let profile_flag = take_value(&mut args, "--profile");

    let command = args.remove(0);
    match command.as_str() {
        "status" => cmd_status(repo_flag.map(|p| paths::expand_tilde(&p))),
        "install" => cmd_install(repo_flag, profile_flag, dry_run, no_build),
        "dev" => cmd_dev(repo_flag, profile_flag, dry_run, no_build),
        "prod" => cmd_prod(repo_flag, dry_run),
        "bundled" | "bundle" => cmd_bundled(dry_run),
        "reload" => cmd_reload(),
        other => Err(format!("未知命令 `{other}`（试试 thunderctl --help）")),
    }
}

fn cmd_status(repo_flag: Option<PathBuf>) -> Result<(), String> {
    let repo = repo_flag.or_else(resolve_repo_hint);
    report::run(repo.as_deref())
}

fn cmd_install(
    repo_flag: Option<String>,
    profile_flag: Option<String>,
    dry_run: bool,
    no_build: bool,
) -> Result<(), String> {
    let repo = require_repo(repo_flag)?;
    let profile = parse_profile(profile_flag, Profile::Release)?;
    let target = build::target_dir(&repo);

    println!("▸ 仓库   {}", repo.display());
    println!("▸ profile {}", profile.as_str());

    if !no_build {
        println!("▸ 构建 {} 个包…", paths::ALL_PACKAGES.len());
        build::build_packages(&repo, profile, &paths::ALL_PACKAGES)?;
    }

    let install_dir = paths::install_dir();
    if dry_run {
        println!("▸ --dry-run：将发布到 {}", install_dir.display());
        for binary in paths::ALL_BINARIES {
            println!(
                "    {} → {binary}",
                target.join(profile.dir_name()).join(binary).display()
            );
        }
        return Ok(());
    }

    for (package, binary) in paths::ALL_PACKAGES.iter().zip(paths::ALL_BINARIES) {
        let mut src = target.join(profile.dir_name()).join(binary);
        if !src.is_file() && binary == "thunderctl" {
            // Never publish a debug `thunderctl` over a release one: prefer the built
            // artifact, fall back to the running binary only when the build is absent.
            src = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
            println!(
                "  · target/{} 里没有 thunderctl，回退到当前运行的二进制",
                profile.dir_name()
            );
        }
        let dest = build::publish(&src, &install_dir, binary)?;
        println!(
            "  ✓ {:<34} ← {:<13} {}",
            dest.display(),
            package,
            report::human_size(proc::file_size(&dest))
        );
    }

    println!();
    println!("完成。接下来：");
    println!("  thunderctl prod      # 让 Desktop 用安装版（默认仍是 App 自带版）");
    println!("  thunderctl status    # 核对解析链");
    Ok(())
}

fn cmd_dev(
    repo_flag: Option<String>,
    profile_flag: Option<String>,
    dry_run: bool,
    no_build: bool,
) -> Result<(), String> {
    let repo = require_repo(repo_flag)?;
    let profile = parse_profile(profile_flag, Profile::TuiDev)?;
    let target = build::target_dir(&repo).join(profile.dir_name());

    if !no_build {
        println!(
            "▸ 构建 {} 个包（profile {}）…",
            paths::DESKTOP_PACKAGES.len(),
            profile.as_str()
        );
        build::build_packages(&repo, profile, &paths::DESKTOP_PACKAGES)?;
    }

    let daemon = target.join("thunder-daemon");
    let tui = target.join("thunder-tui");
    for binary in [&daemon, &tui] {
        if !binary.is_file() && !dry_run {
            return Err(format!(
                "构建产物缺失: {}（先去掉 --no-build 跑一次构建）",
                binary.display()
            ));
        }
    }

    let next = ThunderSettings {
        repo_path: Some(repo.display().to_string()),
        daemon_path: Some(daemon.display().to_string()),
        tui_path: Some(tui.display().to_string()),
    };
    apply("dev", Some(next), dry_run)
}

fn cmd_prod(repo_flag: Option<String>, dry_run: bool) -> Result<(), String> {
    let install_dir = paths::install_dir();
    let daemon = install_dir.join("thunder-daemon");
    let tui = install_dir.join("thunder-tui");
    if !dry_run && (!daemon.is_file() || !tui.is_file()) {
        return Err(format!(
            "{} 里还没有安装版二进制，先执行 `thunderctl install`",
            install_dir.display()
        ));
    }

    let next = ThunderSettings {
        repo_path: repo_flag
            .map(|p| paths::expand_tilde(&p).display().to_string())
            .or_else(|| resolve_repo_hint().map(|p| p.display().to_string())),
        daemon_path: Some(daemon.display().to_string()),
        tui_path: Some(tui.display().to_string()),
    };
    apply("prod", Some(next), dry_run)
}

fn cmd_bundled(dry_run: bool) -> Result<(), String> {
    apply("bundled", None, dry_run)
}

fn cmd_reload() -> Result<(), String> {
    let running = proc::running_daemons();
    if running.is_empty() {
        return Err(
            "没有找到常驻 thunder-daemon（Desktop 会在下次使用 Thunder 会话时自动拉起）".into(),
        );
    }

    let mut reloaded = 0;
    for daemon in &running {
        let Some(state) = daemon.read_state() else {
            println!(
                "· pid {} 没有运行时状态文件（{}）→ 该 daemon 版本过旧，不支持热重启；请重启 App 或重新拉起",
                daemon.pid,
                daemon.state_file().display()
            );
            continue;
        };
        let advertised = state
            .get("hot_reload")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !advertised {
            println!(
                "· pid {} 未声明 hot_reload → 不发送 SIGUSR1（默认动作会直接杀掉进程）",
                daemon.pid
            );
            continue;
        }

        let started_before = state
            .get("started_at_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let disk_mtime = proc::mtime_ms(&daemon.exe);
        println!(
            "▸ pid {} ← {}（磁盘二进制构建于 {}）",
            daemon.pid,
            daemon.exe.display(),
            report::age(disk_mtime)
        );
        proc::request_hot_reload(daemon.pid)?;

        match wait_for_restart(daemon, started_before) {
            Some(state) => {
                reloaded += 1;
                println!(
                    "  ✓ 已热换二进制：v{} · 协议 v{} · pid 不变",
                    state.get("version").and_then(|v| v.as_str()).unwrap_or("?"),
                    state
                        .get("protocol_version")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                );
            }
            None => println!(
                "  ⚠ 3 秒内没有观察到重启（可能有任务在跑，daemon 会拒绝换心）；先让任务结束再试"
            ),
        }
    }

    if reloaded == 0 {
        println!();
        println!("没有 daemon 被替换。需要重启 App 时：完全退出 Agent Resume 再打开。");
    }
    Ok(())
}

fn wait_for_restart(
    daemon: &proc::RunningDaemon,
    started_before: u64,
) -> Option<serde_json::Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(150));
        let state = daemon.read_state()?;
        let started = state
            .get("started_at_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if started != started_before {
            return Some(state);
        }
    }
    None
}

fn apply(mode: &str, next: Option<ThunderSettings>, dry_run: bool) -> Result<(), String> {
    let mut settings = DesktopSettings::load()?;
    let before = settings.thunder();
    let changed = before != next.clone().unwrap_or_default();
    settings.apply(next.as_ref());

    println!("▸ 模式 {mode}");
    println!("▸ 配置文件 {}", settings.file.display());
    match next.as_ref() {
        Some(next) => {
            for (key, value) in [
                ("repoPath", next.repo_path.as_deref()),
                ("daemonPath", next.daemon_path.as_deref()),
                ("tuiPath", next.tui_path.as_deref()),
            ] {
                match value {
                    Some(value) => println!("    {key:<11} {value}"),
                    None => println!("    {key:<11} （清除）"),
                }
            }
        }
        None => println!("    thunder      （整个对象删除 → 交还 App 自带版本）"),
    }

    if dry_run {
        println!("▸ --dry-run：未写入");
        return Ok(());
    }
    if !changed {
        println!("▸ 配置未变化，跳过写入");
    } else {
        let backup = settings.save()?;
        println!("▸ 已写入（备份 {}）", backup.display());
    }

    println!();
    match mode {
        "bundled" => {
            println!(
                "Desktop 下次 spawn 会回落到解析链的下一优先级（通常是 App 自带的 sidecar）。"
            );
            println!("正在跑的 daemon 不会被替换 → 重启 App 生效。");
        }
        _ => {
            println!("Desktop 下次 spawn 会使用上面的路径；正在跑的 daemon 不受影响：");
            println!("  thunderctl reload    # 热换二进制（pid 不变）");
            println!("  完全退出并重开 Agent Resume  # 想要干净重启时");
        }
    }
    println!("  thunderctl status    # 核对解析链");
    Ok(())
}

fn parse_profile(raw: Option<String>, default: Profile) -> Result<Profile, String> {
    match raw {
        None => Ok(default),
        Some(raw) => Profile::parse(&raw)
            .ok_or_else(|| format!("未知 profile `{raw}`（release | tui-dev | dev）")),
    }
}

fn require_repo(repo_flag: Option<String>) -> Result<PathBuf, String> {
    let explicit = repo_flag.map(|p| paths::expand_tilde(&p));
    let repo = explicit.or_else(resolve_repo_hint).ok_or_else(|| {
        "找不到 thunder 仓库根目录：请在仓库内执行，或传 --repo <path> / 设置 THUNDER_PATH"
            .to_string()
    })?;
    if !is_thunder_repo(&repo) {
        return Err(format!(
            "{} 看起来不是 thunder 仓库（缺 Cargo.toml 或 crates/thunder-daemon）",
            repo.display()
        ));
    }
    Ok(repo)
}

fn resolve_repo_hint() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("THUNDER_PATH") {
        let path = paths::expand_tilde(raw.trim());
        if is_thunder_repo(&path) {
            return Some(path);
        }
    }
    if let Ok(settings) = DesktopSettings::load() {
        if let Some(raw) = settings.thunder().repo_path {
            let path = paths::expand_tilde(&raw);
            if is_thunder_repo(&path) {
                return Some(path);
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(repo) = paths::find_repo_root(&cwd) {
            return Some(repo);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(repo) = exe.parent().and_then(paths::find_repo_root) {
            return Some(repo);
        }
    }
    paths::local_checkout_candidates()
        .into_iter()
        .find(|candidate| is_thunder_repo(candidate))
}

fn is_thunder_repo(path: &Path) -> bool {
    path.join("Cargo.toml").is_file() && path.join("crates/thunder-daemon").is_dir()
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    if let Some(index) = args.iter().position(|a| a == name) {
        args.remove(index);
        true
    } else {
        false
    }
}

fn take_value(args: &mut Vec<String>, name: &str) -> Option<String> {
    let index = args.iter().position(|a| a == name)?;
    if index + 1 >= args.len() {
        args.remove(index);
        return None;
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Some(value)
}
