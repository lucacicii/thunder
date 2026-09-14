use crate::config::Topology;
use serde::{Deserialize, Serialize};
use thunder_agent_loop::AgentConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub topology: Topology,
    pub reason: String,
    pub confidence: f32,
}

#[derive(Clone)]
pub struct IntentRouter {
    base_config: Option<AgentConfig>,
}

impl IntentRouter {
    pub fn new(base_config: Option<AgentConfig>) -> Self {
        Self { base_config }
    }

    /// Autonomously classify the user prompt and decide the optimal orchestration topology.
    pub async fn route(&self, prompt: &str, use_mock: bool) -> RoutingDecision {
        if use_mock || self.base_config.is_none() {
            return self.heuristic_route(prompt);
        }

        if let Some(base) = &self.base_config {
            if let Ok(decision) = self.llm_route(prompt, base).await {
                return decision;
            }
        }

        self.heuristic_route(prompt)
    }

    pub fn heuristic_route(&self, prompt: &str) -> RoutingDecision {
        let p_lower = prompt.to_lowercase();

        let parallel_keywords = [
            "review", "audit", "benchmark", "compare", "security", "perf",
            "审查", "评审", "评估", "对比", "安全", "性能", "多视角", "分析",
        ];
        if parallel_keywords.iter().any(|k| p_lower.contains(k)) {
            return RoutingDecision {
                topology: Topology::Parallel,
                reason: "Task requires multi-perspective review, evaluation, or parallel comparison.".to_string(),
                confidence: 0.90,
            };
        }

        let sequential_keywords = [
            "implement", "build", "refactor", "create", "design and", "plan and",
            "health check", "feature", "开发", "实现", "编写", "重构", "设计并", "方案", "流水线",
        ];
        if sequential_keywords.iter().any(|k| p_lower.contains(k)) {
            return RoutingDecision {
                topology: Topology::Sequential,
                reason: "Task requires deliberate architectural planning followed by step-by-step implementation.".to_string(),
                confidence: 0.88,
            };
        }

        RoutingDecision {
            topology: Topology::Single,
            reason: "Standard direct query or atomic task best handled by a single autonomous agent.".to_string(),
            confidence: 0.95,
        }
    }

    async fn llm_route(&self, prompt: &str, base: &AgentConfig) -> Result<RoutingDecision, String> {
        let _ = (prompt, base);
        Err("live LLM routing requires a provider-injected client".to_string())
    }
}
