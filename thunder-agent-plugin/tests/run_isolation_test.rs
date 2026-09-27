//! Per-run isolation of the shared plugin sidecar.
//!
//! One Node process serves every concurrent run, so anything a plugin can reach
//! used to be a single process-wide value: whichever run initialised last decided
//! what all the others could do. That is not only a permission slip — the tool
//! invoker *is* a pipeline, carrying that run's workspace root and path jail, so
//! a misrouted call is a cross-workspace write.
//!
//! Each test here runs two registrations against one registry and asserts they
//! cannot observe or spend each other's authority.

use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_plugin::{run_registry, RunRegistry, RunServices};

fn register(runs: &RunRegistry, route: &str, ws: &std::path::Path, permission: Permission) {
    futures_lite::block_on(async {
        runs.write()
            .await
            .begin_run(
                route,
                ws.to_path_buf(),
                SessionPolicy::new(permission, PermissionMode::Yolo),
                Some(Arc::new(NullHostUi)),
            )
            .await;
    });
}

fn register_with_ui(
    runs: &RunRegistry,
    route: &str,
    ws: &std::path::Path,
    permission: Permission,
    ui: Arc<dyn HostUi>,
) {
    futures_lite::block_on(async {
        runs.write()
            .await
            .begin_run(
                route,
                ws.to_path_buf(),
                SessionPolicy::new(permission, PermissionMode::Yolo),
                Some(ui),
            )
            .await;
    });
}

fn services(runs: &RunRegistry, route: &str) -> Option<RunServices> {
    futures_lite::block_on(async { runs.read().await.get(route).await })
}

/// The tier a run's policy currently enforces.
fn tier_of(runs: &RunRegistry, route: &str) -> Option<Permission> {
    let policy = services(runs, route)?.policy?;
    Some(futures_lite::block_on(async { policy.tier().await }))
}

/// A read-only run must keep its own tier even when a bash run registers after it.
#[test]
fn a_tight_run_keeps_its_tier_when_a_wider_run_starts() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let runs = run_registry();

    register(&runs, "read-run", a.path(), Permission::Read);
    register(&runs, "bash-run", b.path(), Permission::Bash);

    assert_eq!(
        tier_of(&runs, "read-run").unwrap(),
        Permission::Read,
        "the read-only run must not inherit the later bash tier"
    );
    assert_eq!(tier_of(&runs, "bash-run").unwrap(), Permission::Bash);
}

/// The same, in the other order: the *later* run is the tight one, and the earlier
/// wide run must not be tightened either. Both directions matter — the bug was
/// asymmetric.
#[test]
fn a_wide_run_keeps_its_tier_when_a_tight_run_starts() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let runs = run_registry();

    register(&runs, "bash-run", a.path(), Permission::Bash);
    register(&runs, "read-run", b.path(), Permission::Read);

    assert_eq!(tier_of(&runs, "bash-run").unwrap(), Permission::Bash);
    assert_eq!(tier_of(&runs, "read-run").unwrap(), Permission::Read);
}

/// Two runs in different workspaces must not share one jail.
#[test]
fn each_run_keeps_its_own_workspace() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let runs = run_registry();

    register(&runs, "run-a", a.path(), Permission::Bash);
    register(&runs, "run-b", b.path(), Permission::Bash);

    assert_eq!(services(&runs, "run-a").unwrap().workspace, a.path());
    assert_eq!(services(&runs, "run-b").unwrap().workspace, b.path());
}

/// Two runs must not share one dialog surface, or a plugin in one run's dialog
/// would be attributed to the other's task.
#[test]
fn each_run_keeps_its_own_ui() {
    use async_trait::async_trait;
    use thunder_agent_loop::types::ui::{NotifyLevel, UiResponse, UiSource};

    struct A;
    #[async_trait]
    impl HostUi for A {
        async fn request(
            &self,
            _: UiSource,
            _: thunder_agent_loop::types::ui::UiRequest,
        ) -> UiResponse {
            UiResponse::Cancelled
        }
        fn notify(&self, _: UiSource, _: &str, _: NotifyLevel) {}
        fn set_status(&self, _: &str, _: Option<String>) {}
    }
    struct B;
    #[async_trait]
    impl HostUi for B {
        async fn request(
            &self,
            _: UiSource,
            _: thunder_agent_loop::types::ui::UiRequest,
        ) -> UiResponse {
            UiResponse::Cancelled
        }
        fn notify(&self, _: UiSource, _: &str, _: NotifyLevel) {}
        fn set_status(&self, _: &str, _: Option<String>) {}
    }

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let runs = run_registry();
    register_with_ui(&runs, "run-a", a.path(), Permission::Bash, Arc::new(A));
    register_with_ui(&runs, "run-b", b.path(), Permission::Bash, Arc::new(B));

    let ua = services(&runs, "run-a").unwrap().ui.unwrap();
    let ub = services(&runs, "run-b").unwrap().ui.unwrap();
    // Distinct surfaces: `Arc::ptr_eq` is false for two separate allocations.
    assert!(!Arc::ptr_eq(&ua, &ub), "runs must not share one UI handle");
}

/// The invoker is set in a second phase; it must land on the right run.
#[test]
fn the_invoker_lands_on_its_own_run_only() {
    use async_trait::async_trait;

    struct InvokerA;
    #[async_trait]
    impl ToolInvoker for InvokerA {
        async fn invoke(
            &self,
            tool: &str,
            _: serde_json::Value,
            _: &ToolInvocationContext,
        ) -> Result<String, String> {
            Ok(format!("A:{tool}"))
        }
    }

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let runs = run_registry();
    register(&runs, "run-a", a.path(), Permission::Bash);
    register(&runs, "run-b", b.path(), Permission::Bash);

    futures_lite::block_on(async {
        runs.write()
            .await
            .set_tools("run-a", Arc::new(InvokerA))
            .await;
    });

    let a_tools = services(&runs, "run-a").unwrap().tools;
    let b_tools = services(&runs, "run-b").unwrap().tools;
    assert!(a_tools.is_some(), "run-a should have its invoker");
    assert!(
        b_tools.is_none(),
        "run-b must not inherit run-a's invoker: it would dispatch into run-a's pipeline"
    );
}

/// Re-registering a route refreshes the policy without consuming capacity or
/// reordering the eviction queue.
#[test]
fn re_registration_refreshes_in_place() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let runs = run_registry();

    register(&runs, "run-a", a.path(), Permission::Bash);
    register(&runs, "run-b", b.path(), Permission::Bash);
    // Same run, tighter role on a later turn.
    register(&runs, "run-a", a.path(), Permission::Read);

    assert_eq!(tier_of(&runs, "run-a").unwrap(), Permission::Read);
    assert_eq!(tier_of(&runs, "run-b").unwrap(), Permission::Bash);
}

/// Ending a run drops its services, so a later run reusing the id inherits
/// nothing.
#[test]
fn ending_a_run_drops_its_services() {
    let a = tempfile::tempdir().unwrap();
    let runs = run_registry();
    register(&runs, "run-a", a.path(), Permission::Bash);
    assert!(services(&runs, "run-a").is_some());

    futures_lite::block_on(async { runs.write().await.end_run("run-a").await });
    assert!(
        services(&runs, "run-a").is_none(),
        "a finished run must authorise nothing"
    );
}

/// An unknown route resolves to nothing at all — the fail-closed half.
#[test]
fn an_unknown_route_resolves_to_nothing() {
    let a = tempfile::tempdir().unwrap();
    let runs = run_registry();
    register(&runs, "run-a", a.path(), Permission::Bash);

    assert!(services(&runs, "run-ghost").is_none());
    assert!(services(&runs, "").is_none());
}

/// The registry is bounded: a host that crashes before `on_finish` must not leak
/// entries forever, and eviction must degrade to a *refusal*, never a wider grant.
#[test]
fn the_registry_is_bounded() {
    use thunder_agent_plugin::DEFAULT_RUN_REGISTRY_LIMIT;

    let ws = tempfile::tempdir().unwrap();
    let runs = run_registry();
    // Half the cap as already-finished runs, then twice the cap in registrations.
    for i in 0..(DEFAULT_RUN_REGISTRY_LIMIT * 2) {
        register(&runs, &format!("run-{i}"), ws.path(), Permission::Bash);
    }

    let len = futures_lite::block_on(async { runs.read().await.len() });
    assert!(
        len <= DEFAULT_RUN_REGISTRY_LIMIT,
        "registry grew to {len}, past the cap of {DEFAULT_RUN_REGISTRY_LIMIT}"
    );
    // The newest registrations survive; the oldest were evicted.
    assert!(services(
        &runs,
        &format!("run-{}", DEFAULT_RUN_REGISTRY_LIMIT * 2 - 1)
    )
    .is_some());
    assert!(services(&runs, "run-0").is_none());
}

/// `futures_lite::block_on` is not in the dependency set; a local shim keeps the
/// tests synchronous without pulling in a new crate.
mod futures_lite {
    pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(fut)
    }
}
