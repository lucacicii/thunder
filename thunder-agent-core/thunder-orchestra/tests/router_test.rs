use thunder_agent_loop::AgentConfig;
use thunder_orchestra::{IntentRouter, OrchestraConfig, Scheduler, Topology, UnitSpec};

#[tokio::test]
async fn test_heuristic_intent_router_classifications() {
    let router = IntentRouter::new(None);

    // 1. Sequential pipeline intent triggers
    let dec1 = router.heuristic_route("Please implement a user authentication feature with JWT");
    assert_eq!(dec1.topology, Topology::Sequential);
    assert!(dec1.reason.contains("planning followed by"));

    let dec2 = router.heuristic_route("重构数据库连接池并编写单元测试");
    assert_eq!(dec2.topology, Topology::Sequential);

    // 2. Parallel multi-perspective intent triggers
    let dec3 = router.heuristic_route("Review PR #42 for security vulnerabilities and performance");
    assert_eq!(dec3.topology, Topology::Parallel);
    assert!(dec3.reason.contains("multi-perspective review"));

    let dec4 = router.heuristic_route("对比评估 SQLite 与 RocksDB 在高并发场景下的性能");
    assert_eq!(dec4.topology, Topology::Parallel);

    // 3. Fan-out independent subtasks triggers
    let dec_fan = router.heuristic_route("Please decompose and fan out these subtasks in parallel");
    assert_eq!(dec_fan.topology, Topology::FanOut);

    let dec_fan2 = router.heuristic_route("分块并发拆解执行这三批任务");
    assert_eq!(dec_fan2.topology, Topology::FanOut);

    // 4. Single direct query triggers
    let dec5 = router.heuristic_route("What is the latest stable version of Rust?");
    assert_eq!(dec5.topology, Topology::Single);

    let dec6 = router.heuristic_route("hello! How are you?");
    assert_eq!(dec6.topology, Topology::Single);
}

#[tokio::test]
async fn test_scheduler_auto_routing_dispatch() {
    let tmp_dir = std::env::temp_dir().join(format!("test_orch_auto_{}", std::process::id()));
    let base = AgentConfig::new("gpt-4o").with_unlimited_turns();

    let units = vec![
        UnitSpec::new("planner", "planner", base.clone()).with_builtins(),
        UnitSpec::new("coder", "coder", base.clone()).with_builtins(),
        UnitSpec::new("reviewer", "reviewer", base.clone()).with_builtins(),
    ];

    let mut orchestra = OrchestraConfig::new(Topology::Auto)
        .with_store_root(&tmp_dir)
        .with_base(base);

    for unit in units {
        orchestra = orchestra.with_unit(unit);
    }

    let scheduler = Scheduler::new(orchestra);

    // 1. Dispatch multi-step development prompt -> Should auto route to Sequential
    let run_seq = scheduler
        .dispatch("implement a health check interface", true, None)
        .await
        .unwrap();

    assert!(run_seq.routing_decision.is_some());
    let dec = run_seq.routing_decision.unwrap();
    assert_eq!(dec.topology, Topology::Sequential);
    assert_eq!(run_seq.results.len(), 3); // planner + coder + reviewer registered in auto units

    // 2. Dispatch review prompt -> Should auto route to Parallel
    let run_par = scheduler
        .dispatch("review repository performance and memory safety", true, None)
        .await
        .unwrap();

    assert!(run_par.routing_decision.is_some());
    let dec_par = run_par.routing_decision.unwrap();
    assert_eq!(dec_par.topology, Topology::Parallel);

    // Cleanup
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
}
