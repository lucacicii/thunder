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
