use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use thunder_ask::{parse_questions, AskUserToolProvider, MockQuestionHandler, QuestionOption};
use thunder_core::{
    AgentLoopConfig, LlmTurnResponse, ReActAgentLoop, ScriptedMockDriver, ToolCallRequest,
};
use thunder_protocol::{ExecutionContext, ToolProvider};
use thunder_registry::ToolRegistry;

#[test]
fn parser_validates_required_fields_and_options() {
    let valid = json!({
        "questions": [
            {
                "question": "Which database would you prefer?",
                "header": "Database",
                "multi_select": false,
                "options": [
                    { "label": "PostgreSQL", "description": "Relational DB" },
                    { "label": "SQLite", "description": "Embedded DB" }
                ]
            }
        ]
    });

    let items = parse_questions(&valid).expect("valid parse");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].question, "Which database would you prefer?");
    assert_eq!(items[0].header.as_deref(), Some("Database"));
    assert_eq!(items[0].options.len(), 2);
    assert_eq!(
        items[0].options[0],
        QuestionOption {
            label: "PostgreSQL".into(),
            description: Some("Relational DB".into())
        }
    );

    let missing_q = json!({ "questions": [] });
    assert!(parse_questions(&missing_q).is_err());
}

#[tokio::test]
async fn ask_user_provider_delivers_question_and_receives_answer() {
    let ws = tempdir().unwrap();
    let ctx = ExecutionContext::new("sess-ask-1", ws.path(), Duration::from_secs(5));

    let handler = Arc::new(MockQuestionHandler::constant(json!({
        "Which database would you prefer?": "PostgreSQL"
    })));
    let provider = AskUserToolProvider::new(handler);

    let tools = provider.list_tools().await.expect("list_tools");
    assert_eq!(tools[0].name, "core__ask_user_question");

    let res = provider
        .call_tool(
            "core__ask_user_question",
            json!({
                "questions": [
                    {
                        "question": "Which database would you prefer?",
                        "options": [{ "label": "PostgreSQL" }, { "label": "SQLite" }]
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect("call succeeds");

    assert!(res.contains("PostgreSQL"));
}

#[tokio::test]
async fn e2e_agent_loop_asks_user_and_uses_answer_in_subsequent_turn() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();

    let handler = Arc::new(MockQuestionHandler::constant(json!({
        "Which framework?": "React"
    })));
    let ask_provider = AskUserToolProvider::shared(handler);
    registry.register_provider(ask_provider).await.unwrap();

    let mock_driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple(
            "I need to know which framework to configure.",
            vec![ToolCallRequest {
                id: "c-ask-1".into(),
                name: "core__ask_user_question".into(),
                arguments: json!({
                    "questions": [{
                        "question": "Which framework?",
                        "options": [{ "label": "React" }, { "label": "Vue" }]
                    }]
                }),
            }],
        ),
        LlmTurnResponse::simple(
            "Configuring the application with React as selected.",
            vec![],
        ),
    ]));

    let loop_cfg = AgentLoopConfig {
        session_id: "sess-ask-e2e".to_string(),
        workspace_cwd: ws.path().to_path_buf(),
        ..Default::default()
    };

    let mut agent = ReActAgentLoop::new(loop_cfg, registry, mock_driver.clone());
    let outcome = agent.run("Setup web app").await.expect("run succeeds");

    assert_eq!(outcome.turns_executed, 2);
    assert_eq!(
        outcome.final_response,
        "Configuring the application with React as selected."
    );

    // Verify turn 2 received the user's answer
    let turn2_msgs = &mock_driver.recorded_messages()[1];
    let has_answer = turn2_msgs.iter().any(|m| {
        m.content_str()
            .map(|c| c.contains("React"))
            .unwrap_or(false)
    });
    assert!(has_answer, "Turn 2 must receive the user's answer");
}
