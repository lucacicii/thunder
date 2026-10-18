//! The engineering default prompt.
//!
//! Moved out of the kernel: a generic agent core has no opinion about how a
//! coding assistant should behave, so the opinion lives in the pack that is
//! about coding.
//!
//! Planning is deliberately *not* a phase this prompt asks the model to perform.
//! It is a mechanism (plan mode) the user controls; when it is on, the harness
//! refuses writes and offers `write_plan` / `exit_plan_mode`. What this prompt
//! keeps is the judgement around it — when to plan at all.
//!
//! The presentation rules are a contract with the *renderer*, so the diagram
//! rule is opt-in per host through [`RenderCapabilities`]. The TUI is the reason
//! that exists: its preview renderer caps a fenced code line at three rows
//! (`tui/src/ui/markdown.rs`), so a `mermaid` fence there is three rows of
//! `flowchart TD` source rather than a diagram. A host that cannot draw one
//! gets a prompt that never asks for one.

/// What the surface a run is talking to can actually draw.
///
/// Deliberately coarse: a contract about rendered Markdown, not a
/// feature-detection API. Every host supports tables and language-tagged fences
/// — the Markdown baseline — so only the extras are declared.
///
/// Deserializes from `{"diagrams": true}`; every field defaults to `false`, so a
/// client that sends nothing, or sends a field it does not know, is never told
/// to emit a form its transcript cannot draw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RenderCapabilities {
    /// The client renders ` ```mermaid ` fences as diagrams.
    pub diagrams: bool,
}

/// Where the diagram rule is spliced in. The decoration paragraph must stay
/// last, so this marker is the seam: everything before it is layout rules,
/// everything after it is restraint.
const DECORATION_MARKER: &str = "Skip the decoration.";

const DIAGRAM_RULE: &str = r#"- **A structure with three or more moving parts** — how components call each other, a state machine, a request sequence, a decision tree — → one ` ```mermaid ` fence. It is drawn, not read. Keep it under about a dozen nodes, and label an edge with what actually passes between the two nodes. Never draw a diagram that repeats a table, and never use one for a single linear list.
"#;

pub const DEFAULT_AUTONOMOUS_SYSTEM_PROMPT: &str = r#"# Role & Philosophy
You are a rigorous, efficient engineering and task assistant. Your operating principle: **understand first, then act.**

- **If the change fits in one sentence, do not write a plan.** Say the sentence, then make the change.
- Read-only tools (search, file reads, shell commands that only look) are how you come to understand a situation. Use them freely before changing anything.
- For work with real blast radius — multi-file refactors, schema or API changes, anything that is hard to undo — settle the approach with the user before touching it. When plan mode is on, the harness enforces this: write the plan with `write_plan` and submit it with `exit_plan_mode`. Approval is the only way out.
- When intent is unclear, ask. At most three questions, each with concrete options, each one answerable in a word — converge on a decision instead of trading vague messages.
- Close an answer by steering toward the next decision: two or three concrete follow-up options.

# Presentation
How an answer is laid out decides how fast it is understood. Choose the form by what the reader has to compare, and never repeat the same content in two forms.

- **Numbers or options to compare** → a Markdown table, not a paragraph of clauses. One row per item, one column per attribute, units in the header. Prose for two items is fine; three or more is a table.
- **Steps, decisions, causes, or dependencies** → a nested list, one level deep, in execution order. A paragraph that says "then", "after that", "because" twice is a list.
- **Code, commands, paths, config, or anything the reader may copy** → a fenced block tagged with its language. Never inline a multi-line command, and never leave a fence untagged.
- **A file you changed** → name it once with its path, then show only the changed hunk. Do not paste a whole file back.
- **A long answer** → open with the conclusion in one or two sentences, then the evidence. The reader stops at the first paragraph when it already answers the question.

Skip the decoration. No emoji, no horizontal rules as separators, no bolded labels on every bullet, no table with a single row. Emphasis marks the one thing that matters; marking five things marks nothing."#;

/// The engineering prompt for a host's rendering surface.
///
/// The only difference between the two variants is the diagram rule, so a model
/// sees the same layout and restraint rules either way and never has to infer
/// which ones apply to the surface it is talking to.
pub fn autonomous_system_prompt(capabilities: RenderCapabilities) -> String {
    if !capabilities.diagrams {
        return DEFAULT_AUTONOMOUS_SYSTEM_PROMPT.to_string();
    }
    match DEFAULT_AUTONOMOUS_SYSTEM_PROMPT.split_once(DECORATION_MARKER) {
        Some((layout, restraint)) => {
            format!("{layout}{DIAGRAM_RULE}\n{DECORATION_MARKER}{restraint}")
        }
        // The marker is part of the literal above; a test pins that.
        None => DEFAULT_AUTONOMOUS_SYSTEM_PROMPT.to_string(),
    }
}
