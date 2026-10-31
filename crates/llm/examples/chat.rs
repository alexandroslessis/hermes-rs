//! Dev-only: stream one reply from a real OpenAI-compatible endpoint.
//!
//! PowerShell:
//!   $env:HERMES_BASE_URL = "http://localhost:1234/v1"   # LM Studio; Ollama: http://localhost:11434/v1
//!   $env:HERMES_MODEL = "qwen3-8b"
//!   $env:HERMES_API_KEY = "..."                         # optional for local servers
//!   cargo run -p hermes_llm --example chat -- "Say hello in five words"

use futures::StreamExt;
use hermes_llm::api_client::{ApiClient, AuthMethod};
use hermes_llm::base::Provider;
use hermes_llm::conversation::message::Message;
use hermes_llm::model::ModelConfig;
use hermes_llm::openai_compatible::OpenAiCompatibleProvider;
use std::io::Write;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base_url =
        std::env::var("HERMES_BASE_URL").unwrap_or_else(|_| "http://localhost:1234/v1".into());
    let model = std::env::var("HERMES_MODEL")?;
    let auth = match std::env::var("HERMES_API_KEY") {
        Ok(key) => AuthMethod::BearerToken(key),
        Err(_) => AuthMethod::NoAuth,
    };
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Say hello in five words.".into());

    let provider = OpenAiCompatibleProvider::new(
        "dev".into(),
        ApiClient::new_with_tls(base_url, auth, None)?,
        String::new(),
    );
    let mut stream = provider
        .stream(
            &ModelConfig::new(&model),
            "You are a helpful assistant.",
            &[Message::user().with_text(prompt)],
            &[],
        )
        .await?;

    while let Some(item) = stream.next().await {
        let (message, usage) = item?;
        if let Some(message) = message {
            print!("{}", message.as_concat_text());
            std::io::stdout().flush()?;
        }
        if let Some(usage) = usage {
            println!(
                "\n\n[{} in / {} out tokens]",
                usage.usage.input_tokens.unwrap_or(0),
                usage.usage.output_tokens.unwrap_or(0)
            );
        }
    }
    Ok(())
}
