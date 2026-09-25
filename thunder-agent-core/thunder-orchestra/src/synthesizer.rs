use crate::config::UnitSpec;
use std::sync::Arc;
use thunder_agent_loop::{AgentLoop, AgentRunResult, LLMClientTrait};

/// Aggregates and synthesizes outputs from multiple units (e.g. from Parallel Council or Fan-Out).
pub struct Synthesizer;

impl Synthesizer {
    /// Merge and synthesize results into a unified summary.
    ///
    /// Real LLM synthesis happens only when `use_mock == false`, a dedicated
    /// `synthesizer_spec` is configured, **and** the scheduler resolved a live
    /// client for it. Otherwise the unit outputs are returned verbatim under an
    /// honest label — never a fake "successfully synthesized" verdict.
    pub async fn synthesize(
        task_prompt: &str,
        results: &[(String, AgentRunResult)],
        synthesizer_spec: Option<&UnitSpec>,
        client: Option<Arc<dyn LLMClientTrait>>,
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

        let real_synthesis = !use_mock && synthesizer_spec.is_some() && client.is_some();
        if !real_synthesis {
            // Honest degradation: aggregate verbatim, clearly labeled. Do NOT
            // fabricate a "Synthesized Report" or a synthetic conclusion.
            let reason = if use_mock {
                "mock run"
            } else if synthesizer_spec.is_none() {
                "no synthesizer unit configured"
            } else {
                "no LLM client resolved for the synthesizer"
            };
            return Ok(format!(
                "#### Unit Outputs ({})\n\n{}\n_(Aggregated verbatim — LLM synthesis inactive: {reason}.)_",
                results.len(),
                briefs.trim()
            ));
        }

        let spec = synthesizer_spec.expect("checked above");
        let client = client.expect("checked above");

        let synth_prompt = format!(
            "You are the Synthesis and Review Aggregator.\n\
             Original User Task:\n{}\n\n\
             Sub-agent findings and contributions from {} units:\n{}\n\n\
             Synthesize these findings into a unified, coherent, and decisive report. \
             Highlight consensus, resolve discrepancies, and state the final actionable conclusion.",
            task_prompt, results.len(), briefs
        );

        let agent = AgentLoop::new(spec.config.clone())
            .with_id(format!("{}_synthesizer", spec.id))
            .with_custom_client(client);
        let handle = agent.start(synth_prompt, None).map_err(|e| e.to_string())?;
        let res = handle.join().await.map_err(|e| e.to_string())?;
        Ok(res.final_content.unwrap_or_else(|| briefs))
    }
}
