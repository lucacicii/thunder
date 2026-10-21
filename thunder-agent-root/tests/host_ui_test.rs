//! The host UI reaches plugins through `PluginContext`.
//!
//! Two properties are worth pinning:
//!
//! 1. A run without a panel must not hang or lie. `NullHostUi` answers every
//!    dialog with "cancelled", which a plugin has to read as "no".
//! 2. A run with a panel must see *that* panel — the request id and the run's
//!    task binding are what make an approval trustworthy, so a plugin must never
//!    get to pick them.

use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_policy::ui::{HostUi, NotifyLevel, UiRequest, UiResponse, UiSource};
use thunder_agent_root::assembly::{Prepared, RunEnv};
use thunder_agent_root::prelude::*;
use tokio_util::sync::CancellationToken;

/// Answers every dialog with a fixed string and records how it was asked.
struct ScriptedUi {
    answer: &'static str,
    seen: Arc<tokio::sync::Mutex<Vec<(UiSource, String, Vec<String>)>>>,
}

#[async_trait]
impl HostUi for ScriptedUi {
    async fn request(&self, source: UiSource, request: UiRequest) -> UiResponse {
        let (title, options) = match request {
            UiRequest::Select { title, options, .. } => (title, options),
            other => panic!("unexpected request: {other:?}"),
        };
        self.seen
            .lock()
            .await
            .push((source, title.clone(), options.clone()));
        UiResponse::value(self.answer)
    }

    fn notify(&self, _source: UiSource, _message: &str, _level: NotifyLevel) {}
    fn set_status(&self, _key: &str, _text: Option<String>) {}
}

/// Asks the user one question during `on_init` and stores what came back.
struct AskingPlugin {
    seen: Arc<tokio::sync::Mutex<Vec<UiSource>>>,
    answer: Arc<tokio::sync::Mutex<Option<Option<String>>>>,
}

impl Prepared for AskingPlugin {
    fn id(&self) -> &str {
        "asker"
    }

    fn prepare<'a>(
        &'a self,
        env: &'a RunEnv,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let choice = env.ui.select("Pick one", &["alpha", "beta"]).await;
            *self.answer.lock().await = Some(choice);
            self.seen.lock().await.push(UiSource::Host);
            Ok(())
        })
    }
}

/// Emits a single empty completion; the test's subject is the plugin's `on_init`.
struct OneShotClient;

#[async_trait]
impl LLMClientTrait for OneShotClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("done".into()),
                    tool_calls: vec![],
                    finish_reason: "stop".into(),
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

#[tokio::test]
async fn plugin_dialog_reaches_the_runs_host_ui() {
    let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let answer = Arc::new(tokio::sync::Mutex::new(None));

    let ui_seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let root = ThunderRoot::new(AgentConfig::new("mock/ui".to_string()).with_max_turns(2))
        .with_host_ui(Arc::new(ScriptedUi {
            answer: "beta",
            seen: Arc::clone(&ui_seen),
        }))
        .with_prepared(Arc::new(AskingPlugin {
            seen: Arc::clone(&seen),
            answer: Arc::clone(&answer),
        }));

    root.run(
        "hello",
        RootRunOptions {
            session_id: Some("sess-ui".into()),
            custom_client: Some(Arc::new(OneShotClient)),
            register_builtins: false,
            route: None,
            ..Default::default()
        },
    )
    .await
    .expect("run should succeed");

    assert_eq!(
        answer.lock().await.clone(),
        Some(Some("beta".to_string())),
        "the plugin must receive the host's answer"
    );

    let requests = ui_seen.lock().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].0,
        UiSource::Host,
        "host-raised by the agent, not a plugin"
    );
    assert_eq!(requests[0].1, "Pick one");
    assert_eq!(requests[0].2, vec!["alpha", "beta"]);
}

#[tokio::test]
async fn without_a_panel_the_dialog_is_cancelled_not_hung() {
    let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let answer = Arc::new(tokio::sync::Mutex::new(None));

    // No `with_host_ui`: the root falls back to NullHostUi.
    let root = ThunderRoot::new(AgentConfig::new("mock/ui".to_string()).with_max_turns(2))
        .with_prepared(Arc::new(AskingPlugin {
            seen: Arc::clone(&seen),
            answer: Arc::clone(&answer),
        }));

    // A null UI answers instantly, so this must not need a timeout to pass.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.run(
            "hello",
            RootRunOptions {
                session_id: Some("sess-null".into()),
                custom_client: Some(Arc::new(OneShotClient)),
                register_builtins: false,
                route: None,
                ..Default::default()
            },
        ),
    )
    .await
    .expect("a null UI must not block the run");

    assert!(result.is_ok());
    assert_eq!(
        answer.lock().await.clone(),
        Some(None),
        "no panel must resolve to cancelled, which reads as 'no answer'"
    );
}

#[tokio::test]
async fn per_run_ui_overrides_the_root_default() {
    let root_seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let run_seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let answer = Arc::new(tokio::sync::Mutex::new(None));

    let root = ThunderRoot::new(AgentConfig::new("mock/ui".to_string()).with_max_turns(2))
        .with_host_ui(Arc::new(ScriptedUi {
            answer: "alpha",
            seen: Arc::clone(&root_seen),
        }))
        .with_prepared(Arc::new(AskingPlugin {
            seen: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            answer: Arc::clone(&answer),
        }));

    root.run(
        "hello",
        RootRunOptions {
            session_id: Some("sess-override".into()),
            custom_client: Some(Arc::new(OneShotClient)),
            register_builtins: false,
            // The daemon passes a task-scoped handle here.
            ui: Some(Arc::new(ScriptedUi {
                answer: "beta",
                seen: Arc::clone(&run_seen),
            })),
            route: None,
            ..Default::default()
        },
    )
    .await
    .expect("run should succeed");

    assert_eq!(answer.lock().await.clone(), Some(Some("beta".into())));
    assert_eq!(run_seen.lock().await.len(), 1, "the run-scoped UI was used");
    assert!(
        root_seen.lock().await.is_empty(),
        "the root default must not also be consulted"
    );
}

/// Two runs must get two different routes. Sharing one would put them in the same
/// per-run service slot, which is the bug the whole registry exists to prevent.
#[tokio::test]
async fn two_runs_get_distinct_routes() {
    let routes = Arc::new(std::sync::Mutex::new(Vec::new()));

    struct Recorder(Arc<std::sync::Mutex<Vec<String>>>);
    impl Prepared for Recorder {
        fn id(&self) -> &str {
            "rec"
        }

        fn prepare<'a>(
            &'a self,
            env: &'a RunEnv,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>
        {
            Box::pin(async move {
                self.0
                    .lock()
                    .unwrap()
                    .push(env.route.clone().unwrap_or_default());
                Ok(())
            })
        }
    }

    let root = ThunderRoot::new(AgentConfig::new("mock/route".to_string()).with_max_turns(1))
        .with_prepared(Arc::new(Recorder(Arc::clone(&routes))));

    for _ in 0..2 {
        root.run(
            "hello",
            RootRunOptions {
                session_id: Some("sess-same".into()),
                custom_client: Some(Arc::new(OneShotClient)),
                register_builtins: false,
                // Same session, no explicit route: the host must still
                // distinguish the two runs.
                route: None,
                ..Default::default()
            },
        )
        .await
        .expect("run should succeed");
    }

    let seen = routes.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_ne!(seen[0], seen[1], "concurrent runs must not share a route");
}
