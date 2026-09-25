use crate::config::Topology;
use serde::{Deserialize, Serialize};
use thunder_agent_loop::AgentConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub topology: Topology,
    pub reason: String,
    pub confidence: f32,
}

#[derive(Clone, Default)]
pub struct IntentRouter {
    pub base_config: Option<AgentConfig>,
}

impl IntentRouter {
    pub fn new(base_config: Option<AgentConfig>) -> Self {
        Self { base_config }
    }

    /// Autonomously classify the user prompt and decide the optimal orchestration topology
    /// via deterministic intent triggers and structural keywords.
    pub async fn route(&self, prompt: &str, _use_mock: bool) -> RoutingDecision {
        self.heuristic_route(prompt)
    }

    pub fn heuristic_route(&self, prompt: &str) -> RoutingDecision {
        let p_lower = prompt.to_lowercase();

        let fanout_keywords = [
            "fan out", "fanout", "subtask", "subtasks", "partition", "batch process",
            "拆解", "分工", "分块", "分批", "子任务", "分发",
        ];
        if fanout_keywords.iter().any(|k| p_lower.contains(k)) {
            return RoutingDecision {
                topology: Topology::FanOut,
                reason: "Task contains decomposable, independent subtasks suited for fan-out parallelization.".to_string(),
                confidence: 0.92,
            };
        }

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
}
