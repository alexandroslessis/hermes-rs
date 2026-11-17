//! The checks' [`Next`] at each retry and continuation limit (the boundaries `turns.rs` drives
//! through whole turns), and the loop's own helpers.

use std::sync::Arc;

use hermes_agent::operation::{
    ConversationEffect, Emitter, OperationResult, RunStatus, StepResult,
};
use hermes_llm::conversation::Conversation;
use hermes_llm::conversation::message::{Message, MessageContent};
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ErrorData};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::budget::{BudgetKnobs, BudgetProfile, for_profile};
use crate::ops::{
    EMPTY_TERMINAL_MESSAGE, EmptyResponseRecovery, IterationBudget, LengthContinuation,
    SUMMARY_REQUEST, SharedTurnState, TRUNCATED_MESSAGE, TRUNCATED_UNREPORTED_MESSAGE,
    ValidateToolCalls, exit_on_error, no_summary_message,
};

fn emitter() -> Emitter {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // The receiver is leaked so `emit` never fails for want of one (the checks ignore it anyway).
    std::mem::forget(rx);
    Emitter::new(tx, CancellationToken::new())
}

fn with_id(message: Message) -> Message {
    message.with_generated_id_if_missing()
}

fn user(text: &str) -> Message {
    with_id(Message::user().with_text(text))
}

fn call(id: &str, name: &str) -> Message {
    with_id(
        Message::assistant().with_tool_request(
            id,
            Ok(CallToolRequestParams::new(name.to_string())
                .with_arguments(rmcp::object!({"command": "ls"}))),
        ),
    )
}

fn answer(id: &str) -> Message {
    with_id(Message::user().with_tool_response(
        id,
        Ok(CallToolResult::success(vec![ContentBlock::text("done")])),
    ))
}

fn cut_off(mut message: Message) -> Message {
    message.metadata.output_token_limit_reached = true;
    message
}

fn conversation(messages: impl IntoIterator<Item = Message>) -> Conversation {
    Conversation::new_unvalidated(messages)
}

fn next(checked: &Checked) -> Option<Next> {
    checked
        .as_ref()
        .unwrap()
        .as_ref()
        .map(|outcome| outcome.next.clone())
}

fn appended_messages(checked: &Checked) -> Vec<Message> {
    checked
        .as_ref()
        .unwrap()
        .iter()
        .flat_map(|outcome| &outcome.effects)
        .filter_map(|effect| match effect {
            HermesEffect::Conversation(ConversationEffect::AppendMessage(message)) => {
                Some(message.clone())
            }
            _ => None,
        })
        .collect()
}

/// The texts of the messages the effects append.
fn appended(checked: &Checked) -> Vec<String> {
    appended_messages(checked)
        .iter()
        .map(Message::as_concat_text)
        .collect()
}

fn profiles() -> [BudgetKnobs; 3] {
    [
        for_profile(BudgetProfile::Balanced),
        for_profile(BudgetProfile::Requests),
        for_profile(BudgetProfile::Tokens),
    ]
}

#[test]
fn a_cut_off_tool_call_is_retried_up_to_the_knob_with_a_growing_cap_then_refused() {
    for knobs in profiles() {
        let state = SharedTurnState::default();
        let validate = ValidateToolCalls::new(state.clone(), vec!["terminal".into()], &knobs);
        let emit = emitter();
        let mut caps = Vec::new();
        for attempt in 1..=knobs.truncated_tool_retries {
            let conv = conversation([user("go"), cut_off(call("c1", "terminal"))]);
            let checked = validate.check(&conv, &emit);
            assert_eq!(
                next(&checked),
                Some(Next::Retry(Reason::TruncatedToolCall { attempt }))
            );
            // The loop's next `load` takes the cap; a cap left unused grows from itself.
            caps.push(state.lock().max_tokens);
        }
        // boosted_output_cap: base·2ⁿ, at most max(32768, 2·base), from the cap before.
        let mut cap: Option<i32> = None;
        let expected: Vec<Option<i32>> = (1..=knobs.truncated_tool_retries)
            .map(|n| {
                let base = cap.unwrap_or(4096).max(4096);
                cap = Some((base * (1 << n)).min(32768.max(base * 2)));
                cap
            })
            .collect();
        assert_eq!(caps, expected);
        let conv = conversation([user("go"), cut_off(call("c1", "terminal"))]);
        let checked = validate.check(&conv, &emit);
        assert_eq!(next(&checked), Some(Next::Done));
        assert_eq!(appended(&checked), vec![TRUNCATED_MESSAGE.to_string()]);
        // The count starts again after the refusal.
        let conv = conversation([user("go"), cut_off(call("c1", "terminal"))]);
        assert_eq!(
            next(&validate.check(&conv, &emit)),
            Some(Next::Retry(Reason::TruncatedToolCall { attempt: 1 }))
        );
    }
}

#[test]
fn a_reply_is_validated_once() {
    let knobs = BudgetKnobs::BALANCED;
    let validate = ValidateToolCalls::new(Default::default(), vec!["terminal".into()], &knobs);
    let emit = emitter();
    let conv = conversation([user("go"), cut_off(call("c1", "terminal"))]);
    assert!(next(&validate.check(&conv, &emit)).is_some());
    assert_eq!(next(&validate.check(&conv, &emit)), None);
    // A valid call needs nothing.
    let conv = conversation([user("go"), call("c1", "terminal")]);
    assert_eq!(next(&validate.check(&conv, &emit)), None);
}

#[test]
fn invented_tool_names_strike_out_at_the_knob() {
    for knobs in profiles() {
        let validate = ValidateToolCalls::new(Default::default(), vec!["terminal".into()], &knobs);
        let emit = emitter();
        for strike in 1..knobs.invalid_tool_retries {
            let conv = conversation([user("go"), call("c1", "qqzz")]);
            let checked = validate.check(&conv, &emit);
            assert_eq!(
                next(&checked),
                Some(Next::Retry(Reason::InvalidToolName { strike }))
            );
            assert!(
                appended(&checked).is_empty(),
                "UnhandledToolCalls answers them"
            );
        }
        let conv = conversation([user("go"), call("c1", "qqzz")]);
        let checked = validate.check(&conv, &emit);
        assert_eq!(next(&checked), Some(Next::Done));
        let texts = appended(&checked);
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[1], "Model generated invalid tool call: qqzz");
        // A batch with one real call does not strike.
        let conv = conversation([
            user("go"),
            with_id(
                Message::assistant()
                    .with_tool_request(
                        "c1",
                        Ok(CallToolRequestParams::new("qqzz").with_arguments(rmcp::object!({}))),
                    )
                    .with_tool_request(
                        "c2",
                        Ok(CallToolRequestParams::new("terminal")
                            .with_arguments(rmcp::object!({}))),
                    ),
            ),
        ]);
        assert_eq!(next(&validate.check(&conv, &emit)), None);
    }
}

fn malformed(id: &str, error: &str) -> Message {
    with_id(
        Message::assistant()
            .with_tool_request(id, Err(ErrorData::invalid_params(error.to_string(), None))),
    )
}

#[test]
fn malformed_arguments_are_withdrawn_twice_then_answered() {
    let knobs = BudgetKnobs::BALANCED;
    let validate = ValidateToolCalls::new(Default::default(), vec!["terminal".into()], &knobs);
    let emit = emitter();
    for attempt in 1..=2 {
        let conv = conversation([user("go"), malformed("c1", "expected `,` at line 1")]);
        let checked = validate.check(&conv, &emit);
        assert_eq!(
            next(&checked),
            Some(Next::Retry(Reason::InvalidJson { attempt }))
        );
        assert!(appended(&checked).is_empty());
    }
    let conv = conversation([user("go"), malformed("c1", "expected `,` at line 1")]);
    let checked = validate.check(&conv, &emit);
    assert_eq!(
        next(&checked),
        Some(Next::Retry(Reason::InvalidJsonAnswered))
    );
    let answers = appended_messages(&checked);
    assert_eq!(answers.len(), 1);
    assert_eq!(
        answers[0].get_tool_response_ids(),
        ["c1"].into_iter().collect()
    );
    // And the count starts again.
    let conv = conversation([user("go"), malformed("c1", "expected `,` at line 1")]);
    assert_eq!(
        next(&validate.check(&conv, &emit)),
        Some(Next::Retry(Reason::InvalidJson { attempt: 1 }))
    );
}

#[test]
fn arguments_cut_off_in_transit_end_the_turn_at_once() {
    let knobs = BudgetKnobs::BALANCED;
    let validate = ValidateToolCalls::new(Default::default(), vec!["terminal".into()], &knobs);
    let emit = emitter();
    let conv = conversation([
        user("go"),
        malformed(
            "c1",
            "The model's response was truncated: cut off at: {\"command\": \"l",
        ),
    ]);
    let checked = validate.check(&conv, &emit);
    assert_eq!(next(&checked), Some(Next::Done));
    assert_eq!(
        appended(&checked),
        vec![TRUNCATED_UNREPORTED_MESSAGE.to_string()]
    );
}

#[test]
fn a_cut_off_answer_is_continued_up_to_the_knob_then_stitched() {
    for knobs in profiles() {
        let length = LengthContinuation::new(SharedTurnState::default(), &knobs);
        let emit = emitter();
        let mut messages = vec![user("go")];
        let parts: Vec<String> = (1..=knobs.length_continuations)
            .map(|n| format!("part{n}"))
            .collect();
        for (index, part) in parts.iter().enumerate() {
            messages.push(cut_off(with_id(Message::assistant().with_text(part))));
            let checked = length.check(&conversation(messages.clone()), &emit);
            let n = index as u32 + 1;
            if n < knobs.length_continuations {
                assert_eq!(
                    next(&checked),
                    Some(Next::Retry(Reason::LengthContinuation { attempt: n }))
                );
                messages.extend(appended_messages(&checked));
            } else {
                assert_eq!(next(&checked), Some(Next::Done));
                assert_eq!(appended(&checked), vec![parts.join("\n")]);
            }
        }
    }
}

#[test]
fn a_repeating_cut_off_answer_is_not_continued() {
    let length = LengthContinuation::new(SharedTurnState::default(), &BudgetKnobs::BALANCED);
    let text = "the same line again\n".repeat(200);
    let conv = conversation([
        user("go"),
        cut_off(with_id(Message::assistant().with_text(text))),
    ]);
    let checked = length.check(&conv, &emitter());
    assert_eq!(next(&checked), Some(Next::Done));
}

#[test]
fn an_empty_reply_after_tools_gets_one_nudge() {
    let empty = EmptyResponseRecovery {
        state: SharedTurnState::default(),
    };
    let emit = emitter();
    let reply = || with_id(Message::assistant().with_text(""));
    let conv = conversation([user("go"), call("c1", "terminal"), answer("c1"), reply()]);
    let checked = empty.check(&conv, &emit);
    assert_eq!(next(&checked), Some(Next::Retry(Reason::EmptyAfterTools)));
    let conv = conversation([user("go"), call("c1", "terminal"), answer("c1"), reply()]);
    let checked = empty.check(&conv, &emit);
    assert_eq!(next(&checked), Some(Next::Done));
    assert_eq!(appended(&checked), vec![EMPTY_TERMINAL_MESSAGE.to_string()]);
}

#[test]
fn a_thinking_only_reply_is_asked_twice_for_its_answer() {
    let empty = EmptyResponseRecovery {
        state: SharedTurnState::default(),
    };
    let emit = emitter();
    let reply = || with_id(Message::assistant().with_thinking("let me think", ""));
    for attempt in 1..=2 {
        let conv = conversation([user("go"), reply()]);
        assert_eq!(
            next(&empty.check(&conv, &emit)),
            Some(Next::Retry(Reason::ThinkingOnly { attempt }))
        );
    }
    let conv = conversation([user("go"), reply()]);
    assert_eq!(next(&empty.check(&conv, &emit)), Some(Next::Done));
    // A reply with text is not empty.
    let conv = conversation([user("go"), with_id(Message::assistant().with_text("hi"))]);
    assert_eq!(next(&empty.check(&conv, &emit)), None);
}

#[test]
fn text_beside_housekeeping_tools_was_the_answer() {
    let empty = EmptyResponseRecovery {
        state: SharedTurnState::default(),
    };
    let beside = with_id(
        Message::assistant()
            .with_text("All done.")
            .with_tool_request(
                "t1",
                Ok(CallToolRequestParams::new("todo_list").with_arguments(rmcp::object!({}))),
            ),
    );
    let conv = conversation([
        user("go"),
        beside,
        answer("t1"),
        with_id(Message::assistant().with_text("")),
    ]);
    let checked = empty.check(&conv, &emitter());
    assert_eq!(next(&checked), Some(Next::Done));
    assert!(appended(&checked).is_empty());
}

#[test]
fn the_iteration_budget_asks_for_one_summary_and_retries_an_empty_one_once() {
    let state = SharedTurnState::default();
    let budget = IterationBudget {
        state: state.clone(),
        max_iterations: 2,
    };
    let emit = emitter();
    // One model call so far: under the budget.
    let conv = conversation([user("go"), call("c1", "terminal"), answer("c1")]);
    assert_eq!(next(&budget.check(&conv, &emit)), None);
    // Two: the summary is asked for.
    let mut messages = vec![
        user("go"),
        call("c1", "terminal"),
        answer("c1"),
        call("c2", "terminal"),
        answer("c2"),
    ];
    let checked = budget.check(&conversation(messages.clone()), &emit);
    assert_eq!(next(&checked), Some(Next::Retry(Reason::IterationLimit)));
    assert_eq!(appended(&checked), vec![SUMMARY_REQUEST.to_string()]);
    assert!(state.lock().summary_requested);
    messages.extend(appended_messages(&checked));
    // An empty summary is asked again once, then the canned text ends the turn.
    let empty = || with_id(Message::assistant().with_text(""));
    let mut first = messages.clone();
    first.push(empty());
    assert_eq!(
        next(&budget.check(&conversation(first.clone()), &emit)),
        Some(Next::Retry(Reason::EmptySummary))
    );
    first.push(empty());
    let checked = budget.check(&conversation(first), &emit);
    assert_eq!(next(&checked), Some(Next::Done));
    assert_eq!(appended(&checked), vec![no_summary_message(2)]);
    // A summary with text ends the turn as it is.
    messages.push(with_id(
        Message::assistant().with_text("Here is where I got."),
    ));
    let checked = budget.check(&conversation(messages), &emit);
    assert_eq!(next(&checked), Some(Next::Done));
    assert!(appended(&checked).is_empty());
}

#[test]
fn a_provider_error_ends_the_turn() {
    let error = hermes_llm::errors::ProviderError::RequestFailed("boom".into());
    let conv = conversation([user("go"), with_id(Message::from_provider_error(&error))]);
    assert_eq!(next(&exit_on_error(&conv)), Some(Next::Done));
    let conv = conversation([user("go"), with_id(Message::assistant().with_text("hi"))]);
    assert_eq!(next(&exit_on_error(&conv)), None);
}

#[test]
fn agent_crate_steps_map_onto_next() {
    let step = |status| {
        OperationResult::Applied(StepResult {
            effects: Vec::<HermesEffect>::new(),
            applied_step: None,
            status,
        })
    };
    assert!(from_operation(OperationResult::NotApplicable).is_none());
    assert_eq!(
        from_operation(step(RunStatus::Continuing)).unwrap().next,
        Next::Continue
    );
    assert_eq!(
        from_operation(step(RunStatus::Yielded)).unwrap().next,
        Next::Done
    );
}

#[test]
fn stop_answers_each_unanswered_call_once() {
    let twice = with_id(
        Message::assistant()
            .with_tool_request(
                "c2",
                Ok(CallToolRequestParams::new("terminal").with_arguments(rmcp::object!({}))),
            )
            .with_tool_request(
                "c2",
                Ok(CallToolRequestParams::new("terminal").with_arguments(rmcp::object!({}))),
            ),
    );
    let messages: Vec<Arc<Message>> = [user("go"), call("c1", "terminal"), answer("c1"), twice]
        .into_iter()
        .map(Arc::new)
        .collect();
    let response = interrupted_response(&messages).unwrap();
    assert_eq!(
        response.get_tool_response_ids(),
        ["c2"].into_iter().collect()
    );
    let text = response
        .content
        .iter()
        .find_map(MessageContent::as_tool_response)
        .and_then(|r| r.tool_result.as_ref().ok())
        .and_then(|r| r.content.first())
        .and_then(|c| c.as_text().map(|t| t.text.clone()))
        .unwrap();
    assert_eq!(text, "Tool call was interrupted before completing");
    assert!(interrupted_response(&messages[..3]).is_none());
}

// ---- the loop itself: pass order and Stop, over a real `TurnRuntime` -------------------------

mod whole_loop {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use hermes_agent::tool::ToolProvider;
    use hermes_llm::base::{MessageStream, Provider};
    use hermes_llm::errors::ProviderError;
    use hermes_llm::model::ModelConfig;
    use rmcp::model::Tool;

    use super::*;
    use crate::ops::{DeliverSteer, UnhandledToolCalls};
    use crate::session::testing::open_session;

    /// Answers every request with `text` and counts them.
    struct Reply {
        text: &'static str,
        requests: AtomicUsize,
    }

    #[async_trait]
    impl Provider for Reply {
        fn get_name(&self) -> &str {
            "test"
        }

        async fn stream(
            &self,
            _model_config: &ModelConfig,
            _system: &str,
            _messages: &[Arc<Message>],
            _tools: &[Tool],
        ) -> Result<MessageStream, ProviderError> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            let reply = Message::assistant().with_text(self.text);
            Ok(Box::pin(futures::stream::iter(vec![Ok((
                Some(reply),
                None,
            ))])))
        }
    }

    fn reply(text: &'static str) -> Arc<Reply> {
        Arc::new(Reply {
            text,
            requests: AtomicUsize::new(0),
        })
    }

    fn tool(name: &str) -> Tool {
        Tool::new(
            name.to_string(),
            "test tool",
            Arc::new(serde_json::Map::new()),
        )
    }

    /// Offers `echo`, which answers "echoed".
    struct Echo;

    #[async_trait]
    impl ToolProvider<TurnSession> for Echo {
        async fn tools(&self, _session: &TurnSession) -> Result<Vec<Tool>> {
            Ok(vec![tool("echo")])
        }

        async fn call(
            &self,
            _session: &TurnSession,
            _request_id: &str,
            _call: CallToolRequestParams,
            _emit: &Emitter,
        ) -> Result<CallToolResult, ErrorData> {
            Ok(CallToolResult::success(vec![ContentBlock::text("echoed")]))
        }
    }

    /// Listing its tools never finishes; `entered` fires when it starts.
    struct Stalls {
        entered: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl ToolProvider<TurnSession> for Stalls {
        async fn tools(&self, _session: &TurnSession) -> Result<Vec<Tool>> {
            self.entered.notify_one();
            futures::future::pending().await
        }

        async fn call(
            &self,
            _session: &TurnSession,
            _request_id: &str,
            _call: CallToolRequestParams,
            _emit: &Emitter,
        ) -> Result<CallToolResult, ErrorData> {
            unreachable!("never listed")
        }
    }

    /// Listing its tools fails; with `stop` set, Stop is pressed first (in the same poll).
    struct Fails {
        stop: Option<CancellationToken>,
    }

    #[async_trait]
    impl ToolProvider<TurnSession> for Fails {
        async fn tools(&self, _session: &TurnSession) -> Result<Vec<Tool>> {
            if let Some(stop) = &self.stop {
                stop.cancel();
            }
            anyhow::bail!("listing failed")
        }

        async fn call(
            &self,
            _session: &TurnSession,
            _request_id: &str,
            _call: CallToolRequestParams,
            _emit: &Emitter,
        ) -> Result<CallToolResult, ErrorData> {
            unreachable!("never listed")
        }
    }

    struct Setup {
        tools: Arc<dyn ToolProvider<TurnSession>>,
        tool_names: Vec<String>,
        deferred: Option<Arc<crate::deferred::DeferredTools>>,
        max_iterations: u32,
        provider: Arc<Reply>,
        cancel: CancellationToken,
    }

    impl Setup {
        fn new(tools: Arc<dyn ToolProvider<TurnSession>>, names: &[&str]) -> Self {
            Self {
                tools,
                tool_names: names.iter().map(|name| name.to_string()).collect(),
                deferred: None,
                max_iterations: 90,
                provider: reply("final answer"),
                cancel: CancellationToken::new(),
            }
        }

        fn build(&self, state: &SharedTurnState) -> AgentLoop<'static> {
            let knobs = BudgetKnobs::BALANCED;
            AgentLoop {
                budget: IterationBudget {
                    state: state.clone(),
                    max_iterations: self.max_iterations,
                },
                validate: ValidateToolCalls::new(state.clone(), self.tool_names.clone(), &knobs),
                tools: ToolOperation::new().with_provider(self.tools.clone()),
                unhandled: UnhandledToolCalls {
                    tool_names: self.tool_names.clone(),
                    disabled: Default::default(),
                    deferred: self.deferred.clone(),
                },
                steer: Some(DeliverSteer {
                    state: state.clone(),
                }),
                length: LengthContinuation::new(state.clone(), &knobs),
                empty: EmptyResponseRecovery {
                    state: state.clone(),
                },
                compress: None,
                inference: InferenceRunner::new(self.provider.clone(), ModelConfig::new("m")),
                cancel: self.cancel.clone(),
            }
        }
    }

    /// The session's conversation replaced by `messages`.
    fn seed(runtime: &TurnRuntime, messages: impl IntoIterator<Item = Message>) {
        *runtime.session.conversation.lock() = conversation(messages);
    }

    /// Each message's text, a tool answer as `[id: text]`.
    fn texts(runtime: &TurnRuntime) -> Vec<String> {
        runtime
            .session
            .conversation
            .lock()
            .iter()
            .map(|message| {
                let mut text = message.as_concat_text();
                for content in &message.content {
                    if let Some(response) = content.as_tool_response() {
                        let body = response
                            .tool_result
                            .as_ref()
                            .ok()
                            .and_then(|result| result.content.first())
                            .and_then(|content| content.as_text().map(|t| t.text.clone()))
                            .unwrap_or_default();
                        text.push_str(&format!("[{}: {body}]", response.id));
                    }
                }
                text
            })
            .collect()
    }

    const INTERRUPTED: &str = "Tool call was interrupted before completing";

    #[tokio::test]
    async fn stop_during_the_deferred_lookup_answers_the_call_as_interrupted() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (runtime, session) = open_session(dir.path());
        // A deferred session: its frozen prompt holds the sealed block.
        let block = hermes_prompts::deferred::block(&[], &[]);
        let prompt = hermes_prompts::deferred::seal(&format!("identity\n{block}"), &block).unwrap();
        assert!(crate::deferred::is_deferred_session(&prompt));
        session.record.lock().system_prompt = prompt;
        let entered = Arc::new(tokio::sync::Notify::new());
        let mut setup = Setup::new(Arc::new(Echo), &["echo", "ghost"]);
        setup.deferred = Some(Arc::new(crate::deferred::DeferredTools {
            inner: Arc::new(Stalls {
                entered: entered.clone(),
            }),
            disabled: Default::default(),
        }));
        // `ghost` passes validation but nothing offers it: `UnhandledToolCalls` looks it up.
        seed(&runtime, [user("go"), call("g1", "ghost")]);
        let agent = setup.build(&runtime.state);
        let stop = async {
            entered.notified().await;
            setup.cancel.cancel();
        };
        let emit = emitter();
        let (result, ()) = tokio::join!(agent.run(&runtime, &emit), stop);
        result.unwrap();
        assert_eq!(
            texts(&runtime),
            vec![
                "go".to_string(),
                String::new(),
                format!("[g1: {INTERRUPTED}]")
            ]
        );
        assert_eq!(setup.provider.requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_error_after_stop_is_a_stop() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (runtime, _session) = open_session(dir.path());
        let cancel = CancellationToken::new();
        let mut setup = Setup::new(
            Arc::new(Fails {
                stop: Some(cancel.clone()),
            }),
            &["echo"],
        );
        setup.cancel = cancel;
        seed(&runtime, [user("go"), call("c1", "echo")]);
        let agent = setup.build(&runtime.state);
        agent.run(&runtime, &emitter()).await.unwrap();
        assert_eq!(
            texts(&runtime).last().unwrap(),
            &format!("[c1: {INTERRUPTED}]")
        );
    }

    #[tokio::test]
    async fn an_error_without_stop_ends_the_run_as_it_stands() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (runtime, _session) = open_session(dir.path());
        let setup = Setup::new(Arc::new(Fails { stop: None }), &["echo"]);
        seed(&runtime, [user("go"), call("c1", "echo")]);
        let agent = setup.build(&runtime.state);
        let error = agent.run(&runtime, &emitter()).await.unwrap_err();
        assert_eq!(error.to_string(), "listing failed");
        // No finalize: the call stays unanswered.
        assert_eq!(texts(&runtime).len(), 2);
    }

    #[tokio::test]
    async fn unanswered_calls_are_answered_before_a_steer_is_delivered() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (runtime, _session) = open_session(dir.path());
        let setup = Setup::new(Arc::new(Echo), &["echo"]);
        // A batch with a real call (answered) and an invented one (not), and a steer waiting.
        let batch = with_id(
            Message::assistant()
                .with_tool_request(
                    "c1",
                    Ok(CallToolRequestParams::new("echo").with_arguments(rmcp::object!({}))),
                )
                .with_tool_request(
                    "c2",
                    Ok(CallToolRequestParams::new("ghost").with_arguments(rmcp::object!({}))),
                ),
        );
        seed(&runtime, [user("go"), batch, answer("c1")]);
        runtime.state.lock().pending_steer = Some("also check the tests".into());
        let agent = setup.build(&runtime.state);
        let emit = emitter();

        let pass = agent.pass(&runtime.load(), &emit).await.unwrap();
        assert_eq!(pass.status, Status::Continuing);
        let mut effects = pass.effects;
        runtime.apply_effects(&mut effects, &emit).unwrap();
        assert!(texts(&runtime).last().unwrap().starts_with("[c2: "));
        assert!(
            runtime.state.lock().pending_steer.is_some(),
            "the steer waits a pass"
        );

        let pass = agent.pass(&runtime.load(), &emit).await.unwrap();
        let mut effects = pass.effects;
        runtime.apply_effects(&mut effects, &emit).unwrap();
        assert!(runtime.state.lock().pending_steer.is_none());
        assert!(
            texts(&runtime)
                .iter()
                .any(|text| text == "also check the tests")
        );
    }

    #[tokio::test]
    async fn a_spent_budget_lets_pending_calls_run_then_asks_for_the_summary() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (runtime, _session) = open_session(dir.path());
        let mut setup = Setup::new(Arc::new(Echo), &["echo"]);
        setup.max_iterations = 1;
        seed(&runtime, [user("go"), call("c1", "echo")]);
        let agent = setup.build(&runtime.state);
        agent.run(&runtime, &emitter()).await.unwrap();
        let texts = texts(&runtime);
        assert_eq!(texts[2], "[c1: echoed]", "{texts:?}");
        assert_eq!(texts[3], SUMMARY_REQUEST, "{texts:?}");
        assert_eq!(texts.last().unwrap(), "final answer");
        assert_eq!(texts.len(), 5, "{texts:?}");
        // One request: the summary, which ended the turn.
        assert_eq!(setup.provider.requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn each_load_takes_the_one_shot_output_cap() {
        let dir = hermes_testdir::tempdir().unwrap();
        let (runtime, _session) = open_session(dir.path());
        runtime.state.lock().max_tokens = Some(8192);
        assert_eq!(runtime.load().max_tokens, Some(8192));
        assert_eq!(runtime.load().max_tokens, None);
    }
}
