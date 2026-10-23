//! thunder-memory - Autonomous Screen OCR Ingestion, Episodic & Long-Term Memory Engine.

pub mod capture;
pub mod composer_sync;
pub mod config;
pub mod daemon;
pub mod db;
pub mod error;
pub mod mcp;
pub mod search;
pub mod synthesis;

pub use capture::{OcrEngine, RawCaptureJson, ScreenCaptureRecord};
pub use composer_sync::ComposerSyncEngine;
pub use config::MemoryConfig;
pub use daemon::MemoryDaemon;
pub use db::{
    CompactionResult, ComposerTrainingSample, Database, LongTermMemoryRow, MemoryStats,
    NewLongTermMemory, RawCaptureRow, SuppressionMask,
};
pub use error::MemoryError;
pub use mcp::{run_mcp_stdio, MemoryMcpServer};
pub use search::{MemorySearchEngine, ScoredMemory};
pub use synthesis::{CaptureEpisode, SynthesisEngine, SynthesizedInsight};

/// Safely slice a string up to max_bytes without splitting multi-byte UTF-8 character boundaries.
pub fn safe_truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_truncate_chinese_char_boundary() {
        // '钟' is 3 bytes (e.g. at boundary 198..201)
        let s = "这是一段很长的测试中文字符串，包含时钟的钟字".repeat(10);
        for cut in 0..s.len() + 10 {
            let res = safe_truncate_str(&s, cut);
            assert!(res.len() <= cut);
            // Must be valid UTF-8 and not panic
            assert!(std::str::from_utf8(res.as_bytes()).is_ok());
        }
    }
}
