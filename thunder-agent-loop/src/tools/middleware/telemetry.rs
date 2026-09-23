use serde::{Deserialize, Serialize};

/// Structured omniscient telemetry notice informing the LLM of physical realities,
/// security interventions, self-healing events, or execution outcomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemNotice {
    /// Layer or component emitting this telemetry (e.g. "SecurityGuard", "Transaction", "ProcessGuard", "StreamResilience")
    pub layer: String,
    /// Action taken by the system (e.g. "Atomic shadow write & rename", "Blocked path traversal", "Execution cancelled by user")
    pub action: String,
    /// Verified ground truth state of the physical environment (e.g. "Target file untouched, shadow file deleted", "Written 512 bytes")
    pub ground_truth: String,
    /// Explanation of automatic recovery or self-healing performed, if any
    pub self_healed: Option<String>,
    /// Actionable next-step guidance for the LLM
    pub guidance: Option<String>,
}

impl SystemNotice {
    pub fn new(layer: impl Into<String>, action: impl Into<String>, ground_truth: impl Into<String>) -> Self {
        Self {
            layer: layer.into(),
            action: action.into(),
            ground_truth: ground_truth.into(),
            self_healed: None,
            guidance: None,
        }
    }

    pub fn with_self_healed(mut self, healed: impl Into<String>) -> Self {
        self.self_healed = Some(healed.into());
        self
    }

    pub fn with_guidance(mut self, guidance: impl Into<String>) -> Self {
        self.guidance = Some(guidance.into());
        self
    }

    /// Formats the notice into a standardized, prominent Markdown block for the LLM context.
    pub fn format_markdown(&self) -> String {
        let mut out = format!(
            "[System Telemetry: {}\n • Action: {}\n • Ground Truth: {}",
            self.layer, self.action, self.ground_truth
        );
        if let Some(ref healed) = self.self_healed {
            out.push_str(&format!("\n • Self-Healed: {}", healed));
        }
        if let Some(ref guide) = self.guidance {
            out.push_str(&format!("\n • Guidance: {}", guide));
        }
        out.push(']');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_notice_format() {
        let notice = SystemNotice::new(
            "Transaction",
            "Write operation aborted due to timeout",
            "Target file 'src/main.rs' untouched, shadow file deleted",
        )
        .with_guidance("You may safely retry or split your change.");

        let md = notice.format_markdown();
        assert!(md.contains("[System Telemetry: Transaction"));
        assert!(md.contains("• Action: Write operation aborted"));
        assert!(md.contains("• Ground Truth: Target file 'src/main.rs' untouched"));
        assert!(md.contains("• Guidance: You may safely retry"));
    }
}
