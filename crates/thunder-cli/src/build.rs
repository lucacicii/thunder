//! Cargo driver: build the workspace binaries and publish them into the install dir.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Build profile names accepted by `thunderctl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// `cargo --profile release` → `target/release`. What ships.
    Release,
    /// `cargo --profile tui-dev` → `target/tui-dev`. Fast incremental + optimized.
    TuiDev,
    /// `cargo --profile dev` → `target/debug`.
    Dev,
}

impl Profile {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "release" => Some(Profile::Release),
            "tui-dev" | "tuidev" => Some(Profile::TuiDev),
            "dev" | "debug" => Some(Profile::Dev),
            _ => None,
        }
    }

    pub fn cargo_arg(self) -> &'static str {
        match self {
            Profile::Release => "release",
            Profile::TuiDev => "tui-dev",
            Profile::Dev => "dev",
        }
    }

    /// Directory under `target/` that holds the artifacts.
    pub fn dir_name(self) -> &'static str {
        match self {
            Profile::Release => "release",
            Profile::TuiDev => "tui-dev",
            Profile::Dev => "debug",
        }
    }

    pub fn as_str(self) -> &'static str {
        self.dir_name()
    }
}

pub fn target_dir(repo: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(raw) if !raw.is_empty() => {
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                path
            } else {
                repo.join(path)
            }
        }
        _ => repo.join("target"),
    }
}

/// `cargo build --profile <p> -p <packages...>` with inherited stdio so the user
/// watches the same progress they would see by hand.
pub fn build_packages(repo: &Path, profile: Profile, packages: &[&str]) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(&cargo);
    cmd.current_dir(repo)
        .arg("build")
        .arg("--profile")
        .arg(profile.cargo_arg());
    for package in packages {
        cmd.arg("-p").arg(package);
    }
    cmd.stdin(Stdio::inherit());
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run `{cargo} build` in {}: {e}", repo.display()))?;
    if !status.success() {
        return Err(format!("`cargo build` failed with {status}"));
    }
    Ok(())
}

/// Copy one built artifact into the install dir through a temp file + rename, so a
/// running daemon never observes a half-written binary.
pub fn publish(src: &Path, install_dir: &Path, name: &str) -> Result<PathBuf, String> {
    if !src.is_file() {
        return Err(format!("built binary missing: {}", src.display()));
    }
    std::fs::create_dir_all(install_dir)
        .map_err(|e| format!("create {}: {e}", install_dir.display()))?;

    let dest = install_dir.join(name);
    let tmp = install_dir.join(format!(".{name}.thunderctl.tmp"));
    std::fs::copy(src, &tmp).map_err(|e| format!("copy {}: {e}", src.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", tmp.display()))?;
    }

    std::fs::rename(&tmp, &dest).map_err(|e| format!("rename to {}: {e}", dest.display()))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_round_trip() {
        for (raw, profile, dir) in [
            ("release", Profile::Release, "release"),
            ("tui-dev", Profile::TuiDev, "tui-dev"),
            ("dev", Profile::Dev, "debug"),
            ("debug", Profile::Dev, "debug"),
        ] {
            let parsed = Profile::parse(raw).expect("parse");
            assert_eq!(parsed, profile);
            assert_eq!(parsed.dir_name(), dir);
        }
        assert!(Profile::parse("bench").is_none());
    }

    #[test]
    fn publish_writes_executable_atomically() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("thunder-daemon");
        std::fs::write(&src, b"binary").expect("write src");
        let install = tmp.path().join("bin");

        let dest = publish(&src, &install, "thunder-daemon").expect("publish");
        assert_eq!(std::fs::read(&dest).expect("read"), b"binary");
        assert!(!install.join(".thunder-daemon.thunderctl.tmp").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }
}
