use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

/// Initializes the global tracing subscriber for structured logging.
/// Reads configuration from `RUST_LOG` environment variable (e.g. `RUST_LOG=thunder_agent_loop=debug`).
pub fn init_logger() {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .compact()
        .try_init();
}

/// Initializes tracing subscriber with an explicit default level
pub fn init_logger_with_level(default_level: LevelFilter) {
    let filter = EnvFilter::builder()
        .with_default_directive(default_level.into())
        .from_env_lossy();

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .try_init();
}
