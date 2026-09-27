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

#[async_trait]
impl ThunderPlugin for AskingPlugin {
    fn manifest(&self) -> &PluginManifest {
        // A leaked manifest keeps this test focused on the UI path; the plugin
        // is constructed once per test.
        Box::leak(Box::new(
            PluginManifest::new("asker", "Asker", "Asks the user", "0.0.0")
                .with_triggers(TriggerSpec::always()),
        ))
    }

    async fn on_init(
        &self,
        ctx: &PluginContext,
    ) -> Result<(), thunder_agent_root::error::PluginError> {
        let choice = ctx.ui().select("Pick one", &["alpha", "beta"]).await;
        *self.answer.lock().await = Some(choice);
        self.seen.lock().await.push(UiSource::Host);
        Ok(())
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
        .with_plugin(AskingPlugin {
            seen: Arc::clone(&seen),
            answer: Arc::clone(&answer),
        });

    root.run(
        "hello",
        RootRunOptions {
            session_id: Some("sess-ui".into()),
            custom_client: Some(Arc::new(OneShotClient)),
            forced_plugins: Some(vec!["asker".to_string()]),
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
        .with_plugin(AskingPlugin {
            seen: Arc::clone(&seen),
            answer: Arc::clone(&answer),
        });

    // A null UI answers instantly, so this must not need a timeout to pass.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        root.run(
            "hello",
            RootRunOptions {
                session_id: Some("sess-null".into()),
                custom_client: Some(Arc::new(OneShotClient)),
                forced_plugins: Some(vec!["asker".to_string()]),
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
        .with_plugin(AskingPlugin {
            seen: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            answer: Arc::clone(&answer),
        });

    root.run(
        "hello",
        RootRunOptions {
            session_id: Some("sess-override".into()),
            custom_client: Some(Arc::new(OneShotClient)),
            forced_plugins: Some(vec!["asker".to_string()]),
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

/// The two-phase handshake a plugin host needs.
///
/// Phase one (`on_init`) happens before the agent exists, so only identity and
/// policy are available. Phase two (`on_run_ready`) happens once the pipeline is
/// final, and is when a tool invoker can be handed over. A host that skipped the
/// second phase would leave `ctx.callTool` permanently refused.
#[tokio::test]
async fn run_services_arrive_in_two_phases() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Phases {
        init: AtomicUsize,
        ready: AtomicUsize,
        invoker_at_init: AtomicUsize,
        route: std::sync::Mutex<String>,
    }

    struct PhasePlugin(Arc<Phases>);

    #[async_trait]
    impl ThunderPlugin for PhasePlugin {
        fn manifest(&self) -> &PluginManifest {
            Box::leak(Box::new(
                PluginManifest::new("phases", "Phases", "records lifecycle", "0.0.0")
                    .with_triggers(TriggerSpec::always()),
            ))
        }

        async fn on_init(
            &self,
            ctx: &PluginContext,
        ) -> Result<(), thunder_agent_root::error::PluginError> {
            self.0.init.fetch_add(1, Ordering::SeqCst);
            if ctx.tools.0.read().await.is_some() {
                self.0.invoker_at_init.fetch_add(1, Ordering::SeqCst);
            }
            *self.0.route.lock().unwrap() = ctx.route.clone().unwrap_or_default();
            Ok(())
        }

        async fn on_run_ready(
            &self,
            ctx: &PluginContext,
        ) -> Result<(), thunder_agent_root::error::PluginError> {
            self.0.ready.fetch_add(1, Ordering::SeqCst);
            assert!(
                ctx.tools.0.read().await.is_some(),
                "the invoker must exist by the time a plugin is told the run is ready"
            );
            Ok(())
        }
    }

    let phases = Arc::new(Phases {
        init: AtomicUsize::new(0),
        ready: AtomicUsize::new(0),
        invoker_at_init: AtomicUsize::new(0),
        route: std::sync::Mutex::new(String::new()),
    });

    let root = ThunderRoot::new(AgentConfig::new("mock/ready".to_string()).with_max_turns(1))
        .with_plugin(PhasePlugin(Arc::clone(&phases)));

    root.run(
        "hello",
        RootRunOptions {
            session_id: Some("sess-phases".into()),
            custom_client: Some(Arc::new(OneShotClient)),
            forced_plugins: Some(vec!["phases".to_string()]),
            register_builtins: false,
            route: Some("task-42".into()),
            ..Default::default()
        },
    )
    .await
    .expect("run should succeed");

    assert_eq!(phases.init.load(Ordering::SeqCst), 1, "on_init dispatched");
    assert_eq!(
        phases.ready.load(Ordering::SeqCst),
        1,
        "on_run_ready dispatched"
    );
    assert_eq!(
        phases.invoker_at_init.load(Ordering::SeqCst),
        0,
        "the invoker must NOT exist at on_init — the pipeline is not built yet"
    );
    assert_eq!(
        phases.route.lock().unwrap().clone(),
        "task-42",
        "the host's route must reach the plugin verbatim"
    );
}

/// Two runs must get two different routes. Sharing one would put them in the same
/// per-run service slot, which is the bug the whole registry exists to prevent.
#[tokio::test]
async fn two_runs_get_distinct_routes() {
    let routes = Arc::new(std::sync::Mutex::new(Vec::new()));

    struct Recorder(Arc<std::sync::Mutex<Vec<String>>>);
    #[async_trait]
    impl ThunderPlugin for Recorder {
        fn manifest(&self) -> &PluginManifest {
            Box::leak(Box::new(
                PluginManifest::new("rec", "Rec", "records routes", "0.0.0")
                    .with_triggers(TriggerSpec::always()),
            ))
        }
        async fn on_init(
            &self,
            ctx: &PluginContext,
        ) -> Result<(), thunder_agent_root::error::PluginError> {
            self.0
                .lock()
                .unwrap()
                .push(ctx.route.clone().unwrap_or_default());
            Ok(())
        }
    }

    let root = ThunderRoot::new(AgentConfig::new("mock/route".to_string()).with_max_turns(1))
        .with_plugin(Recorder(Arc::clone(&routes)));

    for _ in 0..2 {
        root.run(
            "hello",
            RootRunOptions {
                session_id: Some("sess-same".into()),
                custom_client: Some(Arc::new(OneShotClient)),
                forced_plugins: Some(vec!["rec".to_string()]),
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
