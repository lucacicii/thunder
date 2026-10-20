//! The seam's dependency boundary, checked at build time.
//!
//! The loop is allowed to know two things about tools — their specs and how to
//! call one. Everything else (approval, dangerous-command policy, path scoping,
//! dialogs, traces, scratchpad, plugins) belongs to the host. These assertions
//! keep that boundary from silently eroding: they read the source and fail if a
//! policy type creeps back into the seam or the loop body goes around it.

#[test]
fn the_seam_module_depends_on_no_policy_type() {
    // The `#[cfg(test)]` module is allowed its own `Tools` double.
    let source = include_str!("../src/seam.rs");
    let source = source.split("#[cfg(test)]").next().unwrap_or(source);
    for forbidden in [
        "crate::tools::executor",
        "crate::tools::guard",
        "crate::tools::registry::",
        "crate::types::gate",
        "crate::types::scope",
        "crate::types::ui",
        "crate::types::policy",
        "ToolExecutor",
        "ToolRegistry",
        "impl Tools for",
    ] {
        assert!(
            !source.contains(forbidden),
            "seam.rs must stay policy-free, found `{forbidden}`"
        );
    }
}

#[test]
fn the_loop_executes_only_through_the_seam() {
    let source = include_str!("../src/loop_engine/engine.rs");
    assert!(
        !source.contains(".execute_all("),
        "the loop must not call the executor's batch API directly"
    );
    assert!(
        source.contains("seam::execute_batch("),
        "the loop's tool execution must go through seam::execute_batch"
    );
}

#[test]
fn the_loop_reads_specs_not_the_registry() {
    let source = include_str!("../src/loop_engine/engine.rs");
    assert!(
        !source.contains("tool_registry.get_definitions()"),
        "the request's tool list must come from `Tools::specs()`, not the registry"
    );
    assert!(
        source.contains("tools.specs()"),
        "the loop must ask the seam for specs once per turn"
    );
}
