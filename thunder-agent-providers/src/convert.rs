//! [`ModelDescriptor`] → `rpi_ai::Model`, i.e. the catalog's pi-ai-shaped
//! descriptor rendered into the shape the streaming client actually reads.
//!
//! Most fields move across unchanged. Three need a decision:
//!
//! * **`cost`** — same rates, but thunder passes pricing tiers as opaque JSON
//!   (`Vec<serde_json::Value>`). They are re-parsed into `ModelCostTier`; a tier
//!   that does not fit is dropped rather than failing the whole model, since cost
//!   accounting is advisory and a missing tier only misprices, never strands a call.
//! * **`compat`** — must land in the *typed* variant matching the api. rpi-ai's
//!   providers only read the typed variants (`StreamingProtocolCompat::Other` is
//!   never consulted), so wrapping the JSON opaquely would silently drop every
//!   flag. Unparseable JSON still falls back to `Other` to keep it visible in a
//!   debug dump instead of vanishing.
//! * **`prompt_cache`** — **has no counterpart in rpi-ai's `Model`.** pi-ai
//!   carries per-tier prompt-cache lifetimes on the model; rpi-ai expresses the
//!   same intent through the per-request `CacheRetention`. Thunder derives its
//!   cache-*warming* policy from `promptCache` (`ModelSpec::prompt_cache_warm_settings`)
//!   and that derivation happens on the `ModelSpec`, before this conversion, so
//!   warming is unaffected — but a client built from a descriptor alone has
//!   nothing to read. Reported by [`ConversionNotes`] rather than dropped quietly.

use std::collections::BTreeMap;

use rpi_ai::model::{
    AnthropicMessagesCompat, OpenaiCompletionsCompat, OpenaiResponsesCompat,
    StreamingProtocolCompat,
};
use rpi_ai::{
    Api, InputModality, Model, ModelCost, ModelCostRates, ModelCostTier, ThinkingLevel,
    ThinkingLevelMap,
};
use serde_json::Value;

use crate::model::ModelDescriptor;

/// Things the conversion could not carry across. Returned instead of logged so a
/// caller can decide what to surface.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConversionNotes {
    /// Model-level prompt-cache policy pi-ai declares and rpi-ai has nowhere to put.
    pub dropped_prompt_cache: bool,
    /// Pricing tiers that did not parse into `ModelCostTier`.
    pub dropped_cost_tiers: usize,
    /// `compat` was kept opaque because it did not fit its api's shape.
    pub compat_unparsed: bool,
}

impl ConversionNotes {
    pub fn is_empty(&self) -> bool {
        !self.dropped_prompt_cache && self.dropped_cost_tiers == 0 && !self.compat_unparsed
    }

    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.dropped_prompt_cache {
            out.push("promptCache has no rpi-ai equivalent; the model shape loses it".into());
        }
        if self.dropped_cost_tiers > 0 {
            out.push(format!(
                "{} cost tier(s) did not parse and were dropped",
                self.dropped_cost_tiers
            ));
        }
        if self.compat_unparsed {
            out.push("compat JSON did not fit its api shape; kept opaque".into());
        }
        out
    }
}

/// Which api strings this build can stream. rpi-ai lists more dialects in its
/// `Api` enum than it implements — `google-generative-ai`,
/// `bedrock-converse-stream`, `google-vertex` and `mistral-conversations` are
/// names with no provider behind them — so this gate is the source of truth for
/// "can this model run", and it is deliberately narrower than `Api`.
pub fn supports_api(api: &str) -> bool {
    matches!(
        api,
        "openai-responses" | "openai-completions" | "anthropic-messages"
    )
}

/// The dialects this build streams, for error messages and capability checks.
pub const SUPPORTED_APIS: [&str; 3] = [
    "openai-responses",
    "openai-completions",
    "anthropic-messages",
];

pub fn api_enum(api: &str) -> Api {
    match api {
        "openai-completions" => Api::OpenaiCompletions,
        "openai-responses" => Api::OpenaiResponses,
        "anthropic-messages" => Api::AnthropicMessages,
        "google-generative-ai" => Api::GoogleGenerativeAi,
        other => Api::Other(other.to_string()),
    }
}

pub fn level_from_str(s: &str) -> Option<ThinkingLevel> {
    Some(match s {
        "off" => ThinkingLevel::Off,
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => return None,
    })
}

pub fn model_from_descriptor(model: &ModelDescriptor) -> Result<(Model, ConversionNotes), String> {
    if !supports_api(&model.api) {
        return Err(unsupported_api_message(&model.api));
    }

    let mut notes = ConversionNotes::default();

    let mut out = Model::new(
        model.id.clone(),
        model.name.clone(),
        api_enum(&model.api),
        model.provider.clone(),
        model.base_url.clone(),
    );
    out.reasoning = model.reasoning;
    out.context_window = model.context_window as u64;
    out.max_tokens = model.max_tokens as u64;
    out.input = model
        .input
        .iter()
        .filter_map(|m| match m.as_str() {
            "text" => Some(InputModality::Text),
            "image" => Some(InputModality::Image),
            _ => None,
        })
        .collect();
    if out.input.is_empty() {
        out.input = vec![InputModality::Text];
    }
    out.thinking_level_map = model.thinking_level_map.as_ref().map(|raw| {
        raw.iter()
            .filter_map(|(k, v)| level_from_str(k).map(|lvl| (lvl, v.clone())))
            .collect::<ThinkingLevelMap>()
    });
    if !model.headers.is_empty() {
        out.headers = Some(
            model
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        );
    }
    out.cost = cost(model, &mut notes);
    out.compat = compat(&model.api, model.compat.as_ref(), &mut notes);

    // pi-ai keeps prompt-cache lifetimes on the model; rpi-ai does not.
    notes.dropped_prompt_cache = model.prompt_cache.is_some();

    Ok((out, notes))
}

/// One wording for "this model cannot run here", used by everything that
/// rejects a dialect so a user never has to guess which layer refused.
pub fn unsupported_api_message(api: &str) -> String {
    format!(
        "api `{api}` is not streamable by this build; supported: {}",
        SUPPORTED_APIS.join(", ")
    )
}

fn cost(model: &ModelDescriptor, notes: &mut ConversionNotes) -> ModelCost {
    let Some(cost) = &model.cost else {
        return ModelCost::default();
    };
    let tiers = cost
        .tiers
        .iter()
        .filter_map(
            |tier| match serde_json::from_value::<ModelCostTier>(tier.clone()) {
                Ok(tier) => Some(tier),
                Err(_) => {
                    notes.dropped_cost_tiers += 1;
                    None
                }
            },
        )
        .collect();
    ModelCost {
        rates: ModelCostRates {
            input: cost.input,
            output: cost.output,
            cache_read: cost.cache_read,
            cache_write: cost.cache_write,
        },
        tiers,
    }
}

/// Parse `compat` into the variant its api expects. The typed variants are what
/// rpi-ai's providers actually read; `Other` exists for shapes we cannot name.
fn compat(
    api: &str,
    value: Option<&Value>,
    notes: &mut ConversionNotes,
) -> Option<StreamingProtocolCompat> {
    let value = value?;
    let parsed = match api {
        "anthropic-messages" => serde_json::from_value::<AnthropicMessagesCompat>(value.clone())
            .ok()
            .map(StreamingProtocolCompat::AnthropicMessages),
        "openai-completions" => serde_json::from_value::<OpenaiCompletionsCompat>(value.clone())
            .ok()
            .map(StreamingProtocolCompat::OpenaiCompletions),
        "openai-responses" => serde_json::from_value::<OpenaiResponsesCompat>(value.clone())
            .ok()
            .map(StreamingProtocolCompat::OpenaiResponses),
        _ => None,
    };
    match parsed {
        Some(compat) => Some(compat),
        None => {
            notes.compat_unparsed = true;
            Some(StreamingProtocolCompat::Other(value.clone()))
        }
    }
}
