use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Selectable option inside an agent's question bubble.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Structured question item the agent is blocked on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionItem {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default)]
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

/// Parse and validate the `questions` array parameter from the `ask_user_question` tool arguments.
pub fn parse_questions(args: &Value) -> Result<Vec<QuestionItem>, String> {
    let raw = args
        .get("questions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing required parameter 'questions' (array of questions)".to_string())?;

    if raw.is_empty() {
        return Err("Parameter 'questions' must not be empty".to_string());
    }

    let mut out = Vec::with_capacity(raw.len());
    for (idx, item) in raw.iter().enumerate() {
        let question = item
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("questions[{idx}] is missing required field 'question'"))?
            .to_string();

        let header = item
            .get("header")
            .and_then(|v| v.as_str())
            .map(String::from);

        let multi_select = item
            .get("multiSelect")
            .or_else(|| item.get("multi_select"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let raw_options = item
            .get("options")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                format!("questions[{idx}] is missing required field 'options' (array)")
            })?;

        if raw_options.is_empty() {
            return Err(format!("questions[{idx}] 'options' must not be empty"));
        }

        let mut options = Vec::with_capacity(raw_options.len());
        for (o_idx, opt) in raw_options.iter().enumerate() {
            let label = opt
                .get("label")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("questions[{idx}].options[{o_idx}] is missing 'label'"))?
                .to_string();
            let description = opt
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);
            options.push(QuestionOption { label, description });
        }

        out.push(QuestionItem {
            question,
            header,
            multi_select,
            options,
        });
    }

    Ok(out)
}
