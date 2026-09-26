use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;
use thunder_agent_loop::core::utf8::{safe_slice_from, safe_slice_to};
use thunder_agent_loop::tools::registry::ToolRegistry;
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio_util::sync::CancellationToken;

/// Regression test: truncation of CJK (multi-byte) output must NOT panic.
/// Before the fix, `&output[..keep_side]` panicked with
/// "byte index is not a char boundary" on Chinese content.
#[tokio::test]
async fn test_truncation_with_cjk_content_no_panic() {
    struct LargeCJKTool;

    #[async_trait]
    impl AgentTool for LargeCJKTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new_function(
                "large_cjk",
                "large cjk output",
                json!({ "type": "object", "properties": {} }),
            )
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &ToolExecutionContext,
        ) -> Result<String, String> {
            // ~200KB of Chinese text — byte 26214 lands mid-character
            Ok("雷神代理循环高性能测试。".repeat(10_000))
        }
    }

    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(std::sync::Arc::new(LargeCJKTool));
    registry
        .execute_tool_call(
            &ToolCall::new_function("call_1", "large_cjk", "{}"),
            1,
            CancellationToken::new(),
            None,
        )
        .await;

    // Reaching here without panic means the fix works
}

#[test]
fn test_safe_slice_helpers_multibyte() {
    let cjk = "你好世界，中文测试";
    // All byte offsets, including mid-character ones, must not panic
    for i in 0..=cjk.len() {
        let _ = safe_slice_to(cjk, i);
        let _ = safe_slice_from(cjk, i);
    }
}

#[test]
fn test_safe_slice_roundtrip() {
    // "abc你好def": a=0 b=1 c=2 你=3..6 好=6..9 d=9 e=10 f=11
    let s = "abc你好def";

    // Byte 5 is mid-你 (3..6); retreats to boundary 3
    assert_eq!(safe_slice_to(s, 5), "abc");
    // Byte 4 is also mid-你
    assert_eq!(safe_slice_to(s, 4), "abc");
    // Byte 6 is exactly at 好's boundary
    assert_eq!(safe_slice_to(s, 6), "abc你");

    // 你 occupies bytes 3..6, 好 occupies 6..9
    // From byte 4 (mid-你); advances to boundary 6 (start of 好)
    assert_eq!(safe_slice_from(s, 4), "好def");
    // From byte 3 (exact start of 你)
    assert_eq!(safe_slice_from(s, 3), "你好def");
    // From byte 7 (mid-好 6..9); advances to boundary 9
    assert_eq!(safe_slice_from(s, 7), "def");
}
