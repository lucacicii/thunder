use crate::config::UnitSpec;
use thunder_agent_loop::{AgentLoop, AgentRunResult};

/// Aggregates and synthesizes outputs from multiple units (e.g. from Parallel Council or Fan-Out).
pub struct Synthesizer;

impl Synthesizer {
    /// Merge and synthesize results into a unified summary.
    pub async fn synthesize(
        task_prompt: &str,
        results: &[(String, AgentRunResult)],
        synthesizer_spec: Option<&UnitSpec>,
        use_mock: bool,
    ) -> Result<String, String> {
        if results.is_empty() {
            return Ok("No results to synthesize.".to_string());
        }

        if results.len() == 1 {
            return Ok(results[0].1.final_content.clone().unwrap_or_default());
        }

        // Build brief of all outputs
        let mut briefs = String::new();
        for (role, res) in results {
            briefs.push_str(&format!("### Output from Role `{}` ({:?}):\n", role, res.finish_reason));
            if let Some(content) = &res.final_content {
                briefs.push_str(content.trim());
            } else {
                briefs.push_str("(No content produced)");
            }
            briefs.push_str("\n\n");
        }

        let synth_prompt = format!(
            "You are the Synthesis and Review Aggregator.\n\
             Original User Task:\n{}\n\n\
             Sub-agent findings and contributions from {} units:\n{}\n\n\
             Synthesize these findings into a unified, coherent, and decisive report. \
             Highlight consensus, resolve discrepancies, and state the final actionable conclusion.",
            task_prompt, results.len(), briefs
        );

        if use_mock || synthesizer_spec.is_none() {
            // Deterministic synthesis summary for mock runs or when no dedicated LLM spec is configured
            return Ok(format!(
                "#### [Synthesizer Aggregation Report]\n\
                 **Consolidated from {} units**:\n\n\
                 {}\n\
                 **Conclusion**: Successfully synthesized all unit perspectives into a single unified output.",
                results.len(),
                briefs.trim()
            ));
        }

        let spec = synthesizer_spec.unwrap();
        let agent = AgentLoop::new(spec.config.clone())
            .with_id(format!("{}_synthesizer", spec.id));
        let handle = agent.start(synth_prompt, None).map_err(|e| e.to_string())?;
        let res = handle.join().await.map_err(|e| e.to_string())?;
        Ok(res.final_content.unwrap_or_else(|| briefs))
    }
}
