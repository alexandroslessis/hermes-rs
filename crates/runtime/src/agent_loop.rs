//! The agent loop: one turn (or a review's, a subagent's or the curator's run) as a plain loop
//! over the checks of `ops.rs`, `compress.rs` and the agent crate's tool and inference steps.
//!
//! Goose: `goose/src/agents/state_machine/machine.rs` (Apache-2.0), which ran a `Vec` of
//! `Step` trait objects through `StateMachine::{step, run, finalize}` and was copied as
//! `crates/agent/src/machine.rs`. Phase 13 CP13e replaced it with this module: the same
//! semantics with the plumbing written out (PORTING.md row `diverged`). What did not change,
//! and must not without a reason in this header:
//!
//! - **One pass, one action.** Each pass takes a fresh snapshot of the session
//!   ([`TurnRuntime::load`], which also takes the one-shot output cap a retry asked for), then
//!   asks the checks in this fixed order; the first one with an [`Outcome`] ends the pass, its
//!   effects are applied, and its [`Next`] says whether another pass follows:
//!   iteration budget, tool-call validation, run tools, unanswered calls, `/steer`, length
//!   continuation, empty response, provider error, compression, inference. A pass where no check
//!   has anything to do ends the run. (So the plan's "prepare, infer, validate, run tools,
//!   post-checks, maybe compress" is the life of a turn over several passes; within a pass the
//!   checks that react to the last reply come before the next request, and every pass starts
//!   again from the top: after tools ran, the budget is asked before the next request.)
//! - **Stop.** Before each check the cancel token is looked at; each check is raced against
//!   Stop with `biased` priority for the check, so a check that finishes in the poll that sees
//!   Stop keeps its result (tools rely on it: PITFALLS.md, "After Stop, a tool call must finish
//!   without waiting"), and one still waiting is dropped and remembered as the interrupted check.
//!   Effects of a check that finished are applied even when Stop came meanwhile.
//! - **After Stop** ([`AgentLoop::finalize`]): the interrupted check's leftovers first (the
//!   inference step's streamed output and usage), then a "Tool call was interrupted before
//!   completing" answer for every tool call of the turn nothing answered, then the leftovers of
//!   the other checks in order.
//! - **Errors.** A check's error ends the run with that error (no finalize), unless Stop came
//!   first: then it is a Stop.
//!
//! The retry and continuation rules are the checks' own (`ops.rs`): each returns
//! `Option<Outcome>` (`None`: nothing to do here) with a [`Next`]. Their counters live in the
//! turn's [`crate::ops::TurnState`], not in message metadata. The one note on a message the loop
//! reads is the inference step's empty-response marker (`inference.rs`,
//! `is_empty_response_marker`): it is stored with the hidden message in `sessions.db`, so it stays.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use hermes_agent::inference::InferenceRunner;
use hermes_agent::operation::{
    Emitter, Inference, InferenceInput, MachineEffect, Operation, OperationResult, RunStatus,
    messages_since_kickoff,
};
use hermes_agent::tool::ToolOperation;
use hermes_llm::conversation::message::{Message, MessageContent};
use tokio_util::sync::CancellationToken;

use crate::compress::AutoCompress;
use crate::ops;
use crate::session::{HermesEffect, TurnRuntime, TurnSession};

/// Why a check asked the model again (a recovery or a limit rule of Hermes's turn loop). Counts
/// are 1-based: the first retry is `attempt: 1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The iteration budget is spent: one last toolless call asks for a summary.
    IterationLimit,
    /// The summary came back empty: asked once more.
    EmptySummary,
    /// The output limit cut off a tool call: re-asked with a bigger output cap.
    TruncatedToolCall { attempt: u32 },
    /// A batch of tool calls named only tools that don't exist (`strike` of the allowed ones).
    InvalidToolName { strike: u32 },
    /// A tool call's arguments were not valid JSON: the reply is withdrawn and asked again.
    InvalidJson { attempt: u32 },
    /// The third invalid-JSON reply: every call answered with the parse error instead.
    InvalidJsonAnswered,
    /// A text answer the output limit cut off: the model is asked to continue it.
    LengthContinuation { attempt: u32 },
    /// An empty reply right after tool results: one nudge.
    EmptyAfterTools,
    /// A reply with reasoning but no answer: asked for the answer.
    ThinkingOnly { attempt: u32 },
}

/// What follows a check that acted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// The turn goes on: another pass (the model is asked, or what it asked for is run).
    Continue,
    /// The turn goes on because a rule asks the model again.
    Retry(Reason),
    /// The turn ends here.
    Done,
}

/// A check's result: its effects, applied in order, and what follows.
pub struct Outcome {
    pub effects: Vec<HermesEffect>,
    pub next: Next,
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outcome")
            .field("effects", &self.effects.len())
            .field("next", &self.next)
            .finish()
    }
}

/// What a check returns: `Ok(None)` when it has nothing to do (the next check is asked).
pub type Checked = Result<Option<Outcome>>;

/// The turn goes on with `effects`.
pub fn proceed(effects: impl IntoIterator<Item = HermesEffect>) -> Checked {
    outcome(effects, Next::Continue)
}

/// The model is asked again, for `reason`, after `effects`.
pub fn retry(reason: Reason, effects: impl IntoIterator<Item = HermesEffect>) -> Checked {
    outcome(effects, Next::Retry(reason))
}

/// The turn ends with `effects`.
pub fn done(effects: impl IntoIterator<Item = HermesEffect>) -> Checked {
    outcome(effects, Next::Done)
}

fn outcome(effects: impl IntoIterator<Item = HermesEffect>, next: Next) -> Checked {
    Ok(Some(Outcome {
        effects: effects.into_iter().collect(),
        next,
    }))
}

/// An agent-crate step's result as a check's (`ToolOperation`, `InferenceRunner`).
fn from_operation(result: OperationResult<HermesEffect>) -> Option<Outcome> {
    match result {
        OperationResult::NotApplicable => None,
        OperationResult::Applied(result) => Some(Outcome {
            effects: result.effects,
            next: match result.status {
                RunStatus::Continuing => Next::Continue,
                RunStatus::Yielded | RunStatus::Cancelled => Next::Done,
            },
        }),
    }
}

/// The checks of a pass, in the order they are asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    Budget,
    Validate,
    Tools,
    Unhandled,
    Steer,
    Length,
    Empty,
    ExitOnError,
    Compress,
    Infer,
}

const PASS: [Check; 10] = [
    Check::Budget,
    Check::Validate,
    Check::Tools,
    Check::Unhandled,
    Check::Steer,
    Check::Length,
    Check::Empty,
    Check::ExitOnError,
    Check::Compress,
    Check::Infer,
];

impl Check {
    /// The name Goose's operations had (logs).
    fn name(self) -> &'static str {
        match self {
            Check::Budget => "iteration_budget",
            Check::Validate => "validate_tool_calls",
            Check::Tools => "tools",
            Check::Unhandled => "unhandled_tool_calls",
            Check::Steer => "steer",
            Check::Length => "length_continuation",
            Check::Empty => "empty_response",
            Check::ExitOnError => "exit_on_error",
            Check::Compress => "compress",
            Check::Infer => "llm",
        }
    }
}

/// How a pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Continuing,
    Done,
    /// Stop; `Some` names the check that was still running.
    Cancelled(Option<Check>),
}

struct Pass {
    effects: Vec<HermesEffect>,
    status: Status,
}

/// One run's checks. `steer` and `compress` are absent for the background review and the
/// curator (they never had them).
pub struct AgentLoop<'a> {
    pub budget: ops::IterationBudget,
    pub validate: ops::ValidateToolCalls,
    pub tools: ToolOperation<TurnSession>,
    pub unhandled: ops::UnhandledToolCalls,
    pub steer: Option<ops::DeliverSteer>,
    pub length: ops::LengthContinuation,
    pub empty: ops::EmptyResponseRecovery,
    pub compress: Option<AutoCompress>,
    pub inference: InferenceRunner<'a, TurnSession, HermesEffect>,
    /// The run's Stop.
    pub cancel: CancellationToken,
}

impl AgentLoop<'_> {
    /// Runs passes until one ends the run, then (after Stop) settles what was cut short.
    pub async fn run(&self, runtime: &TurnRuntime, emit: &Emitter) -> Result<()> {
        let status = loop {
            let session = runtime.load();
            let mut pass = self.pass(&session, emit).await?;
            if !pass.effects.is_empty() {
                for effect in &mut pass.effects {
                    effect.ensure_message_ids();
                }
                runtime.apply_effects(&mut pass.effects, emit)?;
            }
            match pass.status {
                Status::Continuing => {}
                status => break status,
            }
        };
        let interrupted = match status {
            Status::Cancelled(check) => check,
            _ => None,
        };
        self.finalize(runtime, interrupted, emit).await
    }

    /// Asks the checks in order; the first that acts ends the pass.
    async fn pass(&self, session: &TurnSession, emit: &Emitter) -> Result<Pass> {
        for check in PASS {
            if (check == Check::Steer && self.steer.is_none())
                || (check == Check::Compress && self.compress.is_none())
            {
                continue;
            }
            if self.cancel.is_cancelled() {
                return Ok(Pass {
                    effects: Vec::new(),
                    status: Status::Cancelled(None),
                });
            }
            let result = tokio::select! {
                biased;
                result = self.check(check, session, emit) => Some(result),
                _ = self.cancel.cancelled() => None,
            };
            let outcome = match result {
                Some(Ok(outcome)) => outcome,
                Some(Err(error)) if !self.cancel.is_cancelled() => return Err(error),
                _ => {
                    return Ok(Pass {
                        effects: Vec::new(),
                        status: Status::Cancelled(Some(check)),
                    });
                }
            };
            let Some(mut outcome) = outcome else {
                continue;
            };
            tracing::debug!(step = check.name(), next = ?outcome.next, "applied step");
            for effect in &mut outcome.effects {
                effect.ensure_message_ids();
            }
            let status = if self.cancel.is_cancelled() {
                Status::Cancelled(None)
            } else if outcome.next == Next::Done {
                Status::Done
            } else {
                Status::Continuing
            };
            return Ok(Pass {
                effects: outcome.effects,
                status,
            });
        }
        Ok(Pass {
            effects: Vec::new(),
            status: Status::Done,
        })
    }

    async fn check(&self, check: Check, session: &TurnSession, emit: &Emitter) -> Checked {
        let conversation = &session.conversation;
        match check {
            Check::Budget => self.budget.check(conversation, emit),
            Check::Validate => self.validate.check(conversation, emit),
            Check::Tools => Ok(from_operation(
                Operation::<TurnSession, HermesEffect>::run(
                    &self.tools,
                    session,
                    conversation,
                    emit,
                )
                .await?,
            )),
            Check::Unhandled => self.unhandled.check(session, conversation, emit).await,
            Check::Steer => match &self.steer {
                Some(steer) => steer.check(conversation, emit),
                None => Ok(None),
            },
            Check::Length => self.length.check(conversation, emit),
            Check::Empty => self.empty.check(conversation, emit),
            Check::ExitOnError => ops::exit_on_error(conversation),
            Check::Compress => match &self.compress {
                Some(compress) => compress.check(session, conversation, emit).await,
                None => Ok(None),
            },
            Check::Infer => self.infer(session, emit).await,
        }
    }

    /// The inference step: a request when the model is next.
    async fn infer(&self, session: &TurnSession, emit: &Emitter) -> Checked {
        let prepared = self.inference.prepare_session(session).await?;
        let session = prepared.as_ref().unwrap_or(session);
        let conversation = &session.conversation;
        if !self.inference.applies(conversation) {
            return Ok(None);
        }
        let input = self.inference_input(session).await?;
        Ok(from_operation(
            self.inference
                .infer(session, conversation, input, emit)
                .await?,
        ))
    }

    /// What the request carries besides the conversation: the session's frozen system prompt
    /// (never changed mid-session, so the provider's prompt cache stays valid) and the tools.
    async fn inference_input(&self, session: &TurnSession) -> Result<InferenceInput> {
        Ok(InferenceInput {
            tools: Operation::<TurnSession, HermesEffect>::inference_tools(&self.tools, session)
                .await?,
            prompt_parts: vec![("system".into(), session.system_prompt.clone())],
            moim_parts: Vec::new(),
        })
    }

    /// A check's leftovers after Stop (only the tool and inference steps keep any).
    async fn leftovers(
        &self,
        check: Check,
        session: &TurnSession,
        emit: &Emitter,
    ) -> Vec<HermesEffect> {
        let conversation = &session.conversation;
        match check {
            Check::Tools => {
                Operation::<TurnSession, HermesEffect>::finalize_cancellation(
                    &self.tools,
                    session,
                    conversation,
                    emit,
                )
                .await
            }
            Check::Infer => {
                Operation::<TurnSession, HermesEffect>::finalize_cancellation(
                    &self.inference,
                    session,
                    conversation,
                    emit,
                )
                .await
            }
            _ => Vec::new(),
        }
    }

    /// After Stop: the interrupted check's leftovers, then answers for the turn's unanswered
    /// tool calls, then the other checks' leftovers. Without Stop, nothing.
    async fn finalize(
        &self,
        runtime: &TurnRuntime,
        interrupted: Option<Check>,
        emit: &Emitter,
    ) -> Result<()> {
        let mut session = runtime.load();
        if !self.cancel.is_cancelled() {
            return Ok(());
        }
        if let Some(check) = interrupted {
            let effects = self.leftovers(check, &session, emit).await;
            session = save(runtime, session, effects, emit)?;
        }
        let unanswered = messages_since_kickoff(&session.conversation)
            .ok()
            .and_then(interrupted_response);
        if let Some(response) = unanswered {
            let effects = vec![HermesEffect::from(emit.message(response))];
            session = save(runtime, session, effects, emit)?;
        }
        for check in PASS {
            if Some(check) != interrupted {
                let effects = self.leftovers(check, &session, emit).await;
                session = save(runtime, session, effects, emit)?;
            }
        }
        Ok(())
    }
}

/// Applies `effects` and reloads the session when there were any.
fn save(
    runtime: &TurnRuntime,
    session: TurnSession,
    mut effects: Vec<HermesEffect>,
    emit: &Emitter,
) -> Result<TurnSession> {
    if effects.is_empty() {
        return Ok(session);
    }
    for effect in &mut effects {
        effect.ensure_message_ids();
    }
    runtime.apply_effects(&mut effects, emit)?;
    Ok(runtime.load())
}

/// A user message answering every tool call of `messages` nothing answered, or `None`.
fn interrupted_response(messages: &[Arc<Message>]) -> Option<Message> {
    let answered = messages
        .iter()
        .flat_map(|message| message.get_tool_response_ids())
        .collect::<HashSet<_>>();
    let mut request_ids = HashSet::new();
    let mut response = Message::user();
    for request in messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(MessageContent::as_tool_request)
    {
        if request_ids.insert(request.id.as_str()) && !answered.contains(request.id.as_str()) {
            response.add_tool_response_with_metadata(
                request.id.clone(),
                Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::ContentBlock::text("Tool call was interrupted before completing"),
                ])),
                request.metadata.as_ref(),
            );
        }
    }
    (!response.get_tool_response_ids().is_empty()).then_some(response)
}

#[cfg(test)]
#[path = "agent_loop_tests.rs"]
mod tests;
