use bytes::Bytes;
use thunder_agent_loop::stream::sse::SSEStreamParser;

#[test]
fn test_sse_parser_text_deltas() {
    let mut parser = SSEStreamParser::new();

    let chunk1 = Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello \"}}]}\n\n");
    let chunk2 = Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"World!\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");

    let deltas1 = parser.feed_chunk(&chunk1);
    assert_eq!(deltas1.len(), 1);
    assert_eq!(deltas1[0].content_delta.as_deref(), Some("Hello "));

    let deltas2 = parser.feed_chunk(&chunk2);
    assert_eq!(deltas2.len(), 1);
    assert_eq!(deltas2[0].content_delta.as_deref(), Some("World!"));
    assert_eq!(deltas2[0].finish_reason.as_deref(), Some("stop"));
}

#[test]
fn test_sse_parser_fragmented_tool_calls() {
    let mut parser = SSEStreamParser::new();

    let chunk1 = Bytes::from_static(
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_abc\",\"function\":{\"name\":\"calc\",\"arguments\":\"\"}}]}}]}\n\n"
    );
    let chunk2 = Bytes::from_static(
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"val\\\":\"}}]}}]}\n\n"
    );
    let chunk3 = Bytes::from_static(
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"42}\"}}]}}]}\n\n"
    );
    let chunk4 = Bytes::from_static(
        b"data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":15}}\n\ndata: [DONE]\n\n"
    );

    parser.feed_chunk(&chunk1);
    parser.feed_chunk(&chunk2);
    parser.feed_chunk(&chunk3);
    let deltas4 = parser.feed_chunk(&chunk4);

    assert_eq!(deltas4.len(), 1);
    assert_eq!(deltas4[0].finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(deltas4[0].prompt_tokens, Some(10));
    assert_eq!(deltas4[0].completion_tokens, Some(15));

    let completed = parser.get_completed_tool_calls();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].id, "call_abc");
    assert_eq!(completed[0].function.name, "calc");
    assert_eq!(completed[0].function.arguments, "{\"val\":42}");
}

#[test]
fn test_sse_parser_usage_with_reasoning_and_cache_tokens() {
    let mut parser = SSEStreamParser::new();

    let chunk = Bytes::from_static(
        b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Thinking carefully...\"}}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50,\"prompt_tokens_details\":{\"cached_tokens\":40},\"completion_tokens_details\":{\"reasoning_tokens\":35}}}\n\n"
    );

    let deltas = parser.feed_chunk(&chunk);
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].reasoning_delta.as_deref(), Some("Thinking carefully..."));
    assert_eq!(deltas[0].prompt_tokens, Some(100));
    assert_eq!(deltas[0].completion_tokens, Some(50));
    assert_eq!(deltas[0].cached_tokens, Some(40));
    assert_eq!(deltas[0].reasoning_tokens, Some(35));
}

