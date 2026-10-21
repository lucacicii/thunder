//! Producing the trace record: redaction, preview, digest, the interceptor and
//! the sink. The record itself ([`ToolTrace`] and friends) lives in
//! `thunder-agent-types`.

pub use thunder_agent_types::trace::{
    AskAnswer, Decision, DenyBy, GuardHit, GuardKind, ToolTrace, ToolTraceSink, TraceResult,
    TraceTiming, TRACE_SCHEMA,
};

use crate::plugin::{CallToolResult, InterceptorPlugin, Plugin};
use crate::tool_policy::ToolExecutionContext;
use std::sync::{Arc, LazyLock};
use thunder_agent_types::policy::{Caller, ToolEffect};
use thunder_agent_types::scope::Scope;

/// What a value under a secret-looking key is replaced with.
pub const REDACTED_KEY: &str = "[redacted:key]";

/// What a secret-looking substring is replaced with.
pub const REDACTED: &str = "[redacted]";

/// Argument keys whose value is a credential. Case-insensitive, substring.
const SECRET_KEYS: [&str; 11] = [
    "api_key",
    "apikey",
    "token",
    "secret",
    "password",
    "credential",
    "authorization",
    "cookie",
    "private_key",
    "access_key",
    "client_secret",
];

/// Argument keys whose value is a document, not a command. The value is kept as
/// a digest plus a short preview: the full text is already on disk where it was
/// written, and a trace that embeds every file the agent has ever written is a
/// trace nobody reads.
const DOCUMENT_KEYS: [&str; 2] = ["content", "preview"];

/// How much of a document key's value is kept.
const DOCUMENT_PREVIEW_CHARS: usize = 200;

/// Secret shapes recognisable without knowing the key they travelled under.
///
/// The output side is where this matters most: `env`, `cat ~/.zshrc` and a read
/// of `.env` all hand a credential to a tool, and the tool's output is what
/// lands in the trace. Bash only scrubs its own child environment, which does
/// nothing for a file that was read.
static SECRET_PATTERNS: LazyLock<Vec<(&'static str, regex::Regex)>> = LazyLock::new(|| {
    let build = |name, pattern: &str| {
        (
            name,
            regex::Regex::new(pattern).expect("built-in secret pattern compiles"),
        )
    };
    vec![
        build("github_token", r"gh[pousr]_[A-Za-z0-9]{20,}"),
        build("api_key", r"sk-[A-Za-z0-9_-]{20,}"),
        build("aws_access_key_id", r"AKIA[0-9A-Z]{16}"),
        build(
            "jwt",
            r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        ),
        build(
            "private_key",
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
        ),
        // `Authorization: Bearer <token>` and the `user:password@host` shape of
        // a credentialed URL.
        build("bearer_token", r"(?i)bearer\s+[A-Za-z0-9._~+/=-]{16,}"),
        build(
            "url_credentials",
            r"(?i)\b[a-z][a-z0-9+.-]*://[^/:\s]+:[^/@\s]+@",
        ),
    ]
});

/// Replace every secret-shaped substring in `text`.
///
/// Returns the redacted text and the names of the rules that fired, so a reader
/// can tell "there was no secret" from "a secret was removed".
pub fn redact_text(text: &str) -> (String, Vec<String>) {
    let mut out = text.to_string();
    let mut fired = Vec::new();
    for (name, pattern) in SECRET_PATTERNS.iter() {
        if pattern.is_match(&out) {
            // A credentialed URL keeps its scheme and host, which is what makes
            // the line readable at all; the userinfo is what goes.
            let replacement = match *name {
                "url_credentials" => "$scheme://[redacted]@",
                _ => REDACTED,
            };
            out = pattern.replace_all(&out, replacement).to_string();
            fired.push((*name).to_string());
        }
    }
    (out, fired)
}

/// A short, redacted head of `text`, cut on a character boundary.
pub fn preview_text(text: &str, max_chars: usize) -> String {
    redact_preview(text, max_chars).0
}

/// [`preview_text`], plus the rules that fired inside it.
pub fn redact_preview(text: &str, max_chars: usize) -> (String, Vec<String>) {
    let head: String = text.chars().take(max_chars).collect();
    let (redacted, fired) = redact_text(&head);
    let preview = if text.chars().count() > max_chars {
        format!("{redacted}… (truncated)")
    } else {
        redacted
    };
    (preview, fired)
}

/// What redacting a call's arguments removed, and why.
#[derive(Debug, Default, Clone)]
pub struct Redactions {
    /// Rule names that fired, so a reader can tell "there was no secret" from
    /// "a secret was removed".
    pub rules: Vec<String>,
    /// The literal values a secret-named key held.
    ///
    /// Kept so they can be withheld from everything *derived* from the call.
    /// A tool that echoes its arguments — or that puts one in an error message —
    /// would otherwise put the value back into the record with the key that
    /// named it stripped away, which is the one place the key rule cannot see.
    pub secrets: Vec<String>,
}

impl Redactions {
    fn finish(mut self) -> Self {
        self.rules.sort();
        self.rules.dedup();
        self.secrets.sort();
        self.secrets.dedup();
        self
    }
}

/// The shortest value worth withholding from derived text.
///
/// Below this, a withheld value would start eating ordinary words: the point of
/// the rule is a credential, and a credential that fits in five characters is
/// not one.
const MIN_WITHHOLDABLE_CHARS: usize = 6;

/// Redact a call's arguments in place.
///
/// Three passes, because a credential reaches an argument three ways: under a
/// key that names it, as a document whose body has to be hashed rather than
/// kept, or inside free text.
pub fn redact_arguments(arguments: &mut serde_json::Value) -> Redactions {
    let mut fired = Redactions::default();
    redact_value(arguments, None, &mut fired);
    fired.finish()
}

/// Replace `secrets` wherever they appear in `text`.
///
/// Returns the text and how many occurrences went.
pub fn withhold(text: &str, secrets: &[String]) -> (String, usize) {
    let mut out = text.to_string();
    let mut count = 0;
    for secret in secrets {
        if secret.chars().count() < MIN_WITHHOLDABLE_CHARS {
            continue;
        }
        if out.contains(secret.as_str()) {
            count += out.matches(secret.as_str()).count();
            out = out.replace(secret.as_str(), REDACTED);
        }
    }
    (out, count)
}

fn redact_value(value: &mut serde_json::Value, key: Option<&str>, fired: &mut Redactions) {
    if let Some(name) = key {
        let lower = name.to_ascii_lowercase();
        if SECRET_KEYS.iter().any(|secret| lower.contains(secret)) {
            if let serde_json::Value::String(secret) = value {
                fired.secrets.push(secret.clone());
            }
            *value = serde_json::Value::String(REDACTED_KEY.to_string());
            fired.rules.push(format!("key:{name}"));
            return;
        }
        if let serde_json::Value::String(text) = value {
            if DOCUMENT_KEYS.contains(&lower.as_str()) {
                let (preview, nested) = redact_text(text);
                fired.rules.extend(nested);
                *value = serde_json::json!({
                    "digest": stable_digest(text.as_bytes()),
                    "bytes": text.len(),
                    "preview": preview_text(&preview, DOCUMENT_PREVIEW_CHARS),
                });
                fired.rules.push(format!("document:{name}"));
                return;
            }
        }
    }

    match value {
        serde_json::Value::String(text) => {
            let (redacted, nested) = redact_text(text);
            if !nested.is_empty() {
                *text = redacted;
                fired.rules.extend(nested);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_value(item, None, fired);
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                let k = k.clone();
                redact_value(v, Some(&k), fired);
            }
        }
        _ => {}
    }
}

/// An [`InterceptorPlugin`] adapter that observes tool completions and records
/// a [`ToolTrace`] into an underlying [`ToolTraceSink`].
#[derive(Clone)]
pub struct TraceInterceptor {
    sink: Arc<dyn ToolTraceSink>,
}

impl TraceInterceptor {
    pub fn new(sink: Arc<dyn ToolTraceSink>) -> Self {
        Self { sink }
    }

    pub fn sink(&self) -> &Arc<dyn ToolTraceSink> {
        &self.sink
    }
}

impl Plugin for TraceInterceptor {
    fn id(&self) -> &str {
        "trace_interceptor"
    }

    fn name(&self) -> &str {
        "Trace Telemetry Interceptor"
    }

    fn description(&self) -> &str {
        "Observes tool execution lifecycle and forwards structured execution traces to a ToolTraceSink."
    }
}

#[async_trait::async_trait]
impl InterceptorPlugin for TraceInterceptor {
    fn priority(&self) -> i32 {
        crate::plugin::priority::OBSERVABILITY
    }

    async fn post_execute(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        result: &mut CallToolResult,
        ctx: &ToolExecutionContext,
    ) {
        let text = result.plain_text();
        let digest = stable_digest(text.as_bytes());
        let now = now_ms();
        let trace = ToolTrace {
            schema: TRACE_SCHEMA,
            trace_id: format!("trace-{now}"),
            session_id: None,
            task_id: None,
            turn: ctx.depth,
            tool_call_id: format!("call-{tool_name}"),
            tool: tool_name.to_string(),
            caller: Caller::Model,
            effect: ToolEffect::of(tool_name),
            mode: None,
            scope: Scope::Unknown,
            arguments: args.clone(),
            decision: Decision::Allow,
            guard: None,
            answer: None,
            result: TraceResult {
                is_error: result.is_err(),
                truncated: false,
                original_bytes: text.len(),
                result_bytes: text.len(),
                output_preview: text,
                output_digest: digest,
            },
            redactions: vec![],
            timing: TraceTiming::default(),
            started_at_ms: now,
            finished_at_ms: now,
        };
        self.sink.record(&trace);
    }
}

/// A digest of `bytes` that is stable across releases: FNV-1a, 64-bit, hex.
///
/// Stability is the whole requirement — two runs over the same content must agree
/// after a version upgrade, which rules out `std`'s `DefaultHasher` (documented
/// as unstable between releases). This is for dedupe and "did this change?"
/// checks, **not** for authenticity; nothing here resists a deliberate collision.
pub fn stable_digest(bytes: &[u8]) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Milliseconds since the Unix epoch, or 0 if the clock is before it.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_trace() -> ToolTrace {
        ToolTrace {
            schema: TRACE_SCHEMA,
            trace_id: "t-1".into(),
            session_id: Some("s-1".into()),
            task_id: None,
            turn: 2,
            tool_call_id: "call_1".into(),
            tool: "write_file".into(),
            caller: Caller::Model,
            effect: ToolEffect::Write,
            mode: Some("ask".into()),
            scope: Scope::OutOfScope,
            arguments: json!({ "path": "/tmp/x" }),
            decision: Decision::Asked,
            guard: None,
            answer: Some(AskAnswer::AllowOnce),
            result: TraceResult {
                is_error: false,
                truncated: false,
                original_bytes: 3,
                result_bytes: 3,
                output_preview: "ok\n".into(),
                output_digest: stable_digest(b"ok\n"),
            },
            redactions: vec![],
            timing: TraceTiming {
                total_ms: 1,
                ..Default::default()
            },
            started_at_ms: 1_700_000_000_000,
            finished_at_ms: 1_700_000_000_001,
        }
    }

    #[test]
    fn a_trace_carries_the_decision_and_the_answer_apart() {
        let trace = sample_trace();
        assert_eq!(trace.decision, Decision::Asked);
        assert_eq!(trace.answer, Some(AskAnswer::AllowOnce));
        assert_eq!(trace.scope, Scope::OutOfScope);
    }

    #[test]
    fn a_guard_refusal_names_its_side() {
        let mut trace = sample_trace();
        trace.decision = Decision::Deny {
            reason: "refused".into(),
            by: DenyBy::Guard,
        };
        trace.guard = Some(GuardHit {
            kind: GuardKind::DangerousCommand,
            pattern: "rm -rf /".into(),
            detail: "rm -rf /".into(),
        });
        trace.answer = None;

        let wire = serde_json::to_value(&trace).unwrap();
        assert_eq!(wire["decision"]["by"], json!("guard"));
        assert_eq!(wire["guard"]["kind"], json!("dangerous_command"));
        assert!(wire.get("answer").is_none(), "absent answers are omitted");
    }

    #[test]
    fn round_trips_through_the_wire() {
        let trace = sample_trace();
        let wire = serde_json::to_string(&trace).unwrap();
        let back: ToolTrace = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, trace);
    }

    #[test]
    fn optional_fields_may_be_missing_in_an_older_record() {
        let wire = json!({
            "schema": 1,
            "trace_id": "t-1",
            "turn": 1,
            "tool_call_id": "call_1",
            "tool": "bash",
            "caller": { "caller": "model" },
            "effect": "exec",
            "scope": "unknown",
            "arguments": {},
            "decision": { "decision": "allow" },
            "result": {
                "is_error": false,
                "truncated": false,
                "original_bytes": 0,
                "result_bytes": 0,
                "output_preview": "",
                "output_digest": ""
            },
            "timing": {},
            "started_at_ms": 0,
            "finished_at_ms": 0
        });

        let trace: ToolTrace = serde_json::from_value(wire).unwrap();
        assert_eq!(trace.session_id, None);
        assert_eq!(trace.mode, None);
        assert_eq!(trace.timing.total_ms, 0);
    }

    #[test]
    fn the_digest_is_stable_and_discriminating() {
        assert_eq!(stable_digest(b"ok\n"), stable_digest(b"ok\n"));
        assert_ne!(stable_digest(b"ok\n"), stable_digest(b"ok"));
        assert_eq!(
            stable_digest(b""),
            "cbf29ce484222325",
            "the FNV-1a offset basis, so a change to the algorithm is caught"
        );
    }
}
