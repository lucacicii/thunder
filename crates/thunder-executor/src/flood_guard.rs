use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// Default per-call output buffer ceiling: 64 KiB (§5.2).
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 65_536;

static OVERFLOW_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FloodGuardedOutput {
    pub text: String,
    pub truncated: bool,
    pub total_bytes: usize,
    pub overflow_log_path: Option<PathBuf>,
}

/// Slices string from start to `end` byte index, retreating to the nearest UTF-8 char boundary.
#[inline]
pub fn safe_slice_to(s: &str, mut end: usize) -> &str {
    if end >= s.len() {
        return s;
    }
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Slices string from `start` byte index to the end, advancing to the nearest UTF-8 char boundary.
#[inline]
pub fn safe_slice_from(s: &str, mut start: usize) -> &str {
    if start == 0 {
        return s;
    }
    if start >= s.len() {
        return "";
    }
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Backward-compatible alias for [`safe_slice_to`].
#[inline]
pub fn utf8_safe_prefix(s: &str, max_bytes: usize) -> &str {
    safe_slice_to(s, max_bytes)
}

/// Continuously drain an async stream until EOF while enforcing the `limit` byte cap (§5.2).
///
/// Key engineering invariants:
/// 1. Continually reads `reader` to EOF so the child process never blocks on a full OS pipe buffer.
/// 2. When `total_bytes <= limit`, returns the full output untouched.
/// 3. When `total_bytes > limit`, writes the complete stream to a log file, and returns
///    **head 40% + tail 40%** (UTF-8 boundary safe) so error stack traces at the end of a log
///    are never lost, followed by `[Output truncated: Exceeded ... bytes limit. Full log written to ...]`.
pub async fn drain_stream_with_flood_guard<R>(
    mut reader: R,
    limit: usize,
    session_id: &str,
    stream_tag: &str,
) -> std::io::Result<FloodGuardedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut initial_buf: Vec<u8> = Vec::with_capacity(limit.min(8192));
    let tail_cap = (limit * 4 / 10).max(64);
    let mut tail_ring: VecDeque<u8> = VecDeque::new();
    let mut total_bytes: usize = 0;
    let mut overflow_file: Option<(PathBuf, File)> = None;
    let mut chunk = [0u8; 8192];

    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        let slice = &chunk[..n];
        total_bytes += n;

        if initial_buf.len() < limit {
            let remaining = limit - initial_buf.len();
            let to_keep = remaining.min(n);
            initial_buf.extend_from_slice(&slice[..to_keep]);

            if n > to_keep {
                let (path, mut file) = create_overflow_log(session_id, stream_tag).await?;
                file.write_all(&initial_buf).await?;
                file.write_all(&slice[to_keep..]).await?;
                overflow_file = Some((path, file));
                push_to_ring(&mut tail_ring, &slice[to_keep..], tail_cap);
            }
        } else {
            if overflow_file.is_none() {
                let (path, mut file) = create_overflow_log(session_id, stream_tag).await?;
                file.write_all(&initial_buf).await?;
                overflow_file = Some((path, file));
            }
            if let Some((_, ref mut file)) = overflow_file {
                file.write_all(slice).await?;
            }
            push_to_ring(&mut tail_ring, slice, tail_cap);
        }
    }

    if let Some((path, mut file)) = overflow_file {
        let _ = file.flush().await;
        let head_cap = (limit * 4 / 10).max(64);
        let head_raw = String::from_utf8_lossy(&initial_buf[..initial_buf.len().min(head_cap)]);
        let safe_head = safe_slice_to(&head_raw, head_cap);

        let tail_vec: Vec<u8> = tail_ring.into_iter().collect();
        let tail_raw = String::from_utf8_lossy(&tail_vec);
        let safe_tail = safe_slice_from(&tail_raw, 0);

        let omitted = total_bytes.saturating_sub(safe_head.len() + safe_tail.len());
        let formatted_limit = format_with_commas(limit);
        let text = format!(
            "{safe_head}\n\n[... {omitted} bytes omitted ...]\n\n{safe_tail}\n[Output truncated: Exceeded {formatted_limit} bytes limit. Full log written to {}]",
            path.display()
        );
        Ok(FloodGuardedOutput {
            text,
            truncated: true,
            total_bytes,
            overflow_log_path: Some(path),
        })
    } else {
        let text = String::from_utf8_lossy(&initial_buf).into_owned();
        Ok(FloodGuardedOutput {
            text,
            truncated: false,
            total_bytes,
            overflow_log_path: None,
        })
    }
}

fn push_to_ring(ring: &mut VecDeque<u8>, bytes: &[u8], cap: usize) {
    if bytes.len() >= cap {
        ring.clear();
        ring.extend(bytes[bytes.len() - cap..].iter().copied());
        return;
    }
    let overflow = (ring.len() + bytes.len()).saturating_sub(cap);
    for _ in 0..overflow {
        ring.pop_front();
    }
    ring.extend(bytes.iter().copied());
}

async fn create_overflow_log(
    session_id: &str,
    stream_tag: &str,
) -> std::io::Result<(PathBuf, File)> {
    let seq = OVERFLOW_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let clean_session: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let filename = format!("thunder_output_{clean_session}_{stream_tag}_{pid}_{seq}.log");
    let path = std::env::temp_dir().join(filename);
    let file = File::create(&path).await?;
    Ok((path, file))
}

fn format_with_commas(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn flood_guard_preserves_head_and_tail_and_spills_full_log() {
        let mut payload = String::from("HEAD_MARKER_START\n");
        payload.push_str(&"M".repeat(150_000));
        payload.push_str("\nTAIL_STACKTRACE_END");

        let cursor = std::io::Cursor::new(payload.clone().into_bytes());
        let res =
            drain_stream_with_flood_guard(cursor, DEFAULT_MAX_OUTPUT_BYTES, "sess-test", "stdout")
                .await
                .expect("drain succeeds");

        assert!(res.truncated);
        assert!(res.text.contains("HEAD_MARKER_START"));
        assert!(res.text.contains("TAIL_STACKTRACE_END"));
        assert!(res
            .text
            .contains("[Output truncated: Exceeded 65,536 bytes limit. Full log written to "));

        let path = res.overflow_log_path.expect("overflow file path");
        let on_disk = tokio::fs::read_to_string(&path)
            .await
            .expect("read overflow file");
        assert_eq!(on_disk.len(), payload.len());
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[test]
    fn utf8_safe_slicing_never_panics_on_cjk() {
        let cjk = "你好世界，这是一段中文测试文本";
        assert_eq!(safe_slice_to(cjk, 4), "你");
        assert_eq!(safe_slice_from("你好世界", 4), "世界");
    }
}
