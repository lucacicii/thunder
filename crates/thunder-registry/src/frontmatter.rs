use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use thunder_executor::ScriptEntry;
use thunder_protocol::ToolDescriptor;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Maximum number of header lines scanned for `@thunder ... @end` frontmatter (§3.2).
pub const MAX_FRONTMATTER_SCAN_LINES: usize = 40;

#[derive(Debug, Deserialize)]
struct RawScriptFrontmatter {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_parameters_schema")]
    parameters: Value,
}

fn default_parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {}
    })
}

/// Parse TSP v1 frontmatter from the first 40 lines of a script text (§3.2).
pub fn parse_frontmatter_str(content: &str) -> Result<Option<ToolDescriptor>, String> {
    let mut in_block = false;
    let mut yaml_lines: Vec<String> = Vec::new();

    for line in content.lines().take(MAX_FRONTMATTER_SCAN_LINES) {
        let trimmed = line.trim();
        if !in_block {
            if is_start_marker(trimmed) {
                in_block = true;
            }
            continue;
        }

        if is_end_marker(trimmed) {
            let yaml_payload = yaml_lines.join("\n");
            let parsed: RawScriptFrontmatter = serde_yaml::from_str(&yaml_payload)
                .map_err(|e| format!("Invalid @thunder YAML frontmatter: {e}"))?;
            if parsed.name.trim().is_empty() {
                return Err("Frontmatter 'name' must not be empty".to_string());
            }
            return Ok(Some(ToolDescriptor::new_script(
                parsed.name.trim(),
                parsed.description.trim(),
                parsed.parameters,
            )));
        }

        yaml_lines.push(strip_comment_prefix(line));
    }

    if in_block {
        Err(
            "Unclosed @thunder frontmatter block within the first 40 lines (missing @end)"
                .to_string(),
        )
    } else {
        Ok(None)
    }
}

/// Read at most the first 40 lines of `path` and parse its `@thunder` frontmatter without executing it.
pub async fn parse_script_file(path: &Path) -> Result<Option<ScriptEntry>, String> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("Failed to open script '{}': {e}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let mut header = String::new();
    let mut count = 0usize;

    while count < MAX_FRONTMATTER_SCAN_LINES {
        match lines.next_line().await {
            Ok(Some(line)) => {
                header.push_str(&line);
                header.push('\n');
                count += 1;
            }
            Ok(None) => break,
            Err(e) => {
                return Err(format!(
                    "Failed reading header of '{}': {e}",
                    path.display()
                ))
            }
        }
    }

    let Some(descriptor) = parse_frontmatter_str(&header)? else {
        return Ok(None);
    };

    Ok(Some(ScriptEntry {
        descriptor,
        script_path: path.to_path_buf(),
    }))
}

/// Scan a directory (`.thunder/tools/`) and return all valid TSP v1 script entries.
pub async fn scan_scripts_dir(dir: &Path) -> std::io::Result<Vec<ScriptEntry>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut read_dir = tokio::fs::read_dir(dir).await?;
    let mut entries = Vec::new();

    while let Some(item) = read_dir.next_entry().await? {
        let path = item.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file_name.starts_with('.') {
            continue;
        }
        let Ok(meta) = item.metadata().await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }

        match parse_script_file(&path).await {
            Ok(Some(entry)) => {
                ensure_executable(&path).await;
                entries.push(entry);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "Skipping malformed TSP script");
            }
        }
    }

    entries.sort_by(|a, b| a.descriptor.name.cmp(&b.descriptor.name));
    Ok(entries)
}

#[cfg(unix)]
async fn ensure_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = tokio::fs::metadata(path).await {
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            let mut perms = meta.permissions();
            perms.set_mode(mode | 0o755);
            let _ = tokio::fs::set_permissions(path, perms).await;
        }
    }
}

#[cfg(not(unix))]
async fn ensure_executable(_path: &Path) {}

fn is_start_marker(trimmed: &str) -> bool {
    matches!(
        trimmed,
        "# @thunder" | "#@thunder" | "// @thunder" | "//@thunder" | "-- @thunder" | "--@thunder"
    )
}

fn is_end_marker(trimmed: &str) -> bool {
    matches!(
        trimmed,
        "# @end" | "#@end" | "// @end" | "//@end" | "-- @end" | "--@end"
    )
}

fn strip_comment_prefix(line: &str) -> String {
    let trimmed_start = line.trim_start();
    for prefix in ["# ", "#", "// ", "//", "-- ", "--"] {
        if let Some(rest) = trimmed_start.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_frontmatter_within_first_40_lines() {
        let script = r#"#!/usr/bin/env bash
# @thunder
# name: git_squash_feature
# description: 自动压缩当前分支的所有特性提交并推送到远端
# parameters:
#   type: object
#   properties:
#     base_branch:
#       type: string
#       description: 目标基准分支，例如 main
#   required: [base_branch]
# @end

set -euo pipefail
echo "done"
"#;

        let desc = parse_frontmatter_str(script)
            .expect("valid frontmatter")
            .expect("found descriptor");
        assert_eq!(desc.name, "script__git_squash_feature");
        assert_eq!(desc.uri, "script://git_squash_feature");
        assert_eq!(
            desc.description,
            "自动压缩当前分支的所有特性提交并推送到远端"
        );
        assert_eq!(desc.input_schema["required"][0], "base_branch");
    }

    #[test]
    fn ignores_frontmatter_after_line_40() {
        let mut lines: Vec<String> = (0..42).map(|i| format!("# filler line {i}")).collect();
        lines.push("# @thunder".to_string());
        lines.push("# name: too_late".to_string());
        lines.push("# @end".to_string());
        let content = lines.join("\n");

        let res = parse_frontmatter_str(&content).expect("parse ok");
        assert!(res.is_none());
    }
}
