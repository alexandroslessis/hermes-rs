//! Dev-only: run full agent turns against a real OpenAI-compatible endpoint (default OpenRouter),
//! in the current directory, with streaming output. Sessions go to `%APPDATA%\hermes-rs-dev`.
//!
//! PowerShell:
//!   $env:OPENROUTER_API_KEY = "sk-or-..."
//!   cargo run -p hermes_runtime --example agent -- --models            # list models
//!   $env:HERMES_MODEL = "anthropic/claude-sonnet-4.5"
//!   cargo run -p hermes_runtime --example agent -- "List the files here and summarize the README"
//!
//! Other endpoints: set HERMES_BASE_URL (e.g. http://localhost:1234/v1) and HERMES_API_KEY.
//! Pass `--session <id>` to continue an earlier session.

use std::io::Write as _;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use hermes_agent::events::AgentEvent;
use hermes_llm::conversation::message::MessageContent;
use hermes_runtime::{
    ClarifyAnswer, ClarifyOutcome, ClarifyRequest, ClarifyResponse, ProcessNotice, ProviderConfig,
    Runtime, RuntimeConfig, RuntimeEvent,
};
use hermes_tools::UserBridge;

/// Answers `clarify` on the console.
struct ConsoleBridge;

#[async_trait]
impl UserBridge for ConsoleBridge {
    async fn clarify(&self, _session_id: &str, request: ClarifyRequest) -> ClarifyOutcome {
        let mut responses = Vec::new();
        for question in request.questions {
            println!("\n? {}", question.question);
            for (index, choice) in question.choices.iter().enumerate() {
                println!("  {}. {choice}", index + 1);
            }
            print!("> ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            let answer = tokio::task::spawn_blocking(move || {
                std::io::stdin().read_line(&mut line).ok().map(|_| line)
            })
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
            let answer = answer.trim().to_string();
            let picked = answer
                .parse::<usize>()
                .ok()
                .and_then(|n| question.choices.get(n.wrapping_sub(1)).cloned())
                .unwrap_or(answer.clone());
            responses.push(if answer.is_empty() {
                ClarifyResponse::Skipped
            } else {
                ClarifyResponse::Answered(ClarifyAnswer::Text(picked))
            });
        }
        ClarifyOutcome::Submitted(responses)
    }

    fn process_finished(&self, _session_id: &str, notice: ProcessNotice) {
        println!(
            "\n[background {} exited with {:?}]",
            notice.process_id, notice.exit_code
        );
    }
}

fn clip(text: &str, max: usize) -> String {
    let one_line = text.replace('\n', " ⏎ ");
    if one_line.chars().count() > max {
        format!("{}…", one_line.chars().take(max).collect::<String>())
    } else {
        one_line
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let provider = ProviderConfig {
        id: "openrouter".into(),
        base_url: std::env::var("HERMES_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api/v1".into()),
        api_key: std::env::var("HERMES_API_KEY")
            .or_else(|_| std::env::var("OPENROUTER_API_KEY"))
            .ok(),
    };

    if args.first().map(String::as_str) == Some("--models") {
        let models = provider.list_models().await?;
        println!("{} models", models.len());
        for model in models {
            println!(
                "{:60} ctx={:>8} tools={:<5} in=${:<8} out=${}",
                model.id,
                model
                    .context_length
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into()),
                model
                    .supports_tools
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "?".into()),
                model
                    .input_price
                    .map(|p| format!("{p:.2}"))
                    .unwrap_or_else(|| "?".into()),
                model
                    .output_price
                    .map(|p| format!("{p:.2}"))
                    .unwrap_or_else(|| "?".into()),
            );
        }
        return Ok(());
    }

    let session_arg = args.iter().position(|arg| arg == "--session").map(|index| {
        args.remove(index);
        args.remove(index)
    });
    let prompt = args.join(" ");
    anyhow::ensure!(!prompt.is_empty(), "pass a prompt, or --models");
    let model = std::env::var("HERMES_MODEL").context("set HERMES_MODEL")?;

    let mut config = RuntimeConfig::for_current_user()?;
    config.data_dir = config.data_dir.with_file_name("hermes-rs-dev");
    let runtime = Runtime::new(config, Arc::new(ConsoleBridge))?;
    let context_length = provider
        .list_models()
        .await
        .ok()
        .and_then(|models| models.into_iter().find(|m| m.id == model))
        .and_then(|m| m.context_length);
    let session_id = match session_arg {
        Some(id) => id,
        None => {
            runtime
                .create_session(&provider, &model, context_length, &std::env::current_dir()?)?
                .id
        }
    };
    println!("session {session_id}");

    let mut turn = runtime.start_turn(&session_id, &provider, context_length, &prompt)?;
    let cancel = turn.cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        cancel.cancel();
    });
    while let Some(event) = turn.events.recv().await {
        match event {
            RuntimeEvent::Agent(AgentEvent::Message(message))
                if message.role == rmcp::model::Role::Assistant =>
            {
                for content in &message.content {
                    match content {
                        MessageContent::Text(text) => print!("{}", text.text),
                        MessageContent::Thinking(thinking) => {
                            print!("\x1b[2m{}\x1b[0m", thinking.thinking)
                        }
                        MessageContent::Error(error) => {
                            println!("\n\x1b[31m[error] {}\x1b[0m", error.message)
                        }
                        MessageContent::ToolRequest(request) => {
                            if let Ok(call) = &request.tool_call {
                                let args =
                                    serde_json::to_string(&call.arguments).unwrap_or_default();
                                println!("\n→ {}({})", call.name, clip(&args, 160));
                            }
                        }
                        _ => {}
                    }
                }
                let _ = std::io::stdout().flush();
            }
            RuntimeEvent::Agent(AgentEvent::Message(message)) => {
                for content in &message.content {
                    if let Some(text) = content.as_tool_response_text() {
                        println!("← {}", clip(&text, 200));
                    }
                }
            }
            RuntimeEvent::Agent(AgentEvent::Usage(usage)) => {
                println!(
                    "\n[{} in ({} cached) / {} out tokens]",
                    usage.usage.input_tokens.unwrap_or(0),
                    usage.usage.cache_read_input_tokens.unwrap_or(0),
                    usage.usage.output_tokens.unwrap_or(0)
                );
            }
            RuntimeEvent::TurnFinished(status) => {
                println!("\n[turn finished: {status:?}]");
                break;
            }
            _ => {}
        }
    }
    runtime.close(&session_id).await;
    Ok(())
}
