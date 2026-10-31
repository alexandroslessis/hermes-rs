//! clarify: ask the user questions and wait for the app to return their answers.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{SessionTools, ToolOutput, parse_args};

const MAX_QUESTIONS: usize = 5;
const MAX_CHOICES: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarifyQuestion {
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClarifyRequest {
    pub questions: Vec<ClarifyQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClarifyAnswer {
    Text(String),
    Choices(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClarifyResponse {
    Answered(ClarifyAnswer),
    Skipped,
    Unanswered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClarifyOutcome {
    /// One response per question, in order.
    Submitted(Vec<ClarifyResponse>),
    Cancelled,
    TimedOut,
    /// No UI could show the questions (headless use).
    Undelivered,
}

#[derive(Deserialize)]
struct ClarifyArgs {
    questions: Vec<ClarifyQuestion>,
}

pub async fn clarify(tools: &SessionTools, args: Value) -> ToolOutput {
    let args: ClarifyArgs = match parse_args("clarify", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    if args.questions.is_empty() || args.questions.len() > MAX_QUESTIONS {
        return ToolOutput::error(format!("Pass 1-{MAX_QUESTIONS} questions."));
    }
    if let Some(question) = args
        .questions
        .iter()
        .find(|q| q.choices.len() > MAX_CHOICES)
    {
        return ToolOutput::error(format!(
            "Question '{}' has {} choices; the limit is {MAX_CHOICES} (an 'Other' row is added automatically).",
            question.question,
            question.choices.len()
        ));
    }
    let questions = args.questions.clone();
    let outcome = tools
        .bridge
        .clarify(
            &tools.session_id,
            ClarifyRequest {
                questions: args.questions,
            },
        )
        .await;
    ToolOutput::ok(render(&questions, outcome))
}

fn render(questions: &[ClarifyQuestion], outcome: ClarifyOutcome) -> Value {
    let (label, responses, notice) = match outcome {
        ClarifyOutcome::Submitted(responses) => ("submitted", responses, None),
        ClarifyOutcome::Cancelled => (
            "cancelled",
            Vec::new(),
            Some("The user closed the questions without answering."),
        ),
        ClarifyOutcome::TimedOut => (
            "timed_out",
            Vec::new(),
            Some("The user did not answer in time."),
        ),
        ClarifyOutcome::Undelivered => (
            "undelivered",
            Vec::new(),
            Some(
                "No interface was available to show the questions. Decide yourself and state your assumption.",
            ),
        ),
    };
    let rendered: Vec<Value> = questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let (status, answer) = match responses
                .get(index)
                .cloned()
                .unwrap_or(ClarifyResponse::Unanswered)
            {
                ClarifyResponse::Answered(answer) => ("answered", json!(answer)),
                ClarifyResponse::Skipped => ("skipped", Value::Null),
                ClarifyResponse::Unanswered => ("unanswered", Value::Null),
            };
            json!({"question": question.question, "status": status, "user_response": answer})
        })
        .collect();
    let mut value = json!({"responses": rendered, "outcome": label});
    if let Some(notice) = notice {
        value["notice"] = json!(notice);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::tools_in;

    #[tokio::test]
    async fn headless_clarify_reports_undelivered() {
        let (_dir, tools) = tools_in();
        let out = clarify(
            &tools,
            json!({"questions": [{"question": "Which DB?", "choices": ["sqlite", "postgres"]}]}),
        )
        .await;
        assert_eq!(out.value["outcome"], "undelivered");
        assert_eq!(out.value["responses"][0]["status"], "unanswered");
    }

    #[test]
    fn submitted_answers_render_in_question_order() {
        let questions = vec![
            ClarifyQuestion {
                question: "A?".into(),
                choices: vec![],
                multi_select: false,
            },
            ClarifyQuestion {
                question: "B?".into(),
                choices: vec!["x".into(), "y".into()],
                multi_select: true,
            },
        ];
        let value = render(
            &questions,
            ClarifyOutcome::Submitted(vec![
                ClarifyResponse::Answered(ClarifyAnswer::Text("free text".into())),
                ClarifyResponse::Answered(ClarifyAnswer::Choices(vec!["x".into(), "y".into()])),
            ]),
        );
        assert_eq!(value["responses"][0]["user_response"], "free text");
        assert_eq!(value["responses"][1]["user_response"], json!(["x", "y"]));
        assert_eq!(value["outcome"], "submitted");
    }

    #[tokio::test]
    async fn too_many_choices_is_rejected() {
        let (_dir, tools) = tools_in();
        let out = clarify(
            &tools,
            json!({"questions": [{"question": "?", "choices": ["a", "b", "c", "d", "e"]}]}),
        )
        .await;
        assert!(out.is_error);
    }
}
