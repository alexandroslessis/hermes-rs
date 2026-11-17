//! Dev-only: prints everything our code reads from the model registry, for a list of
//! (provider, model) pairs, so two builds of `hermes_llm` can be compared line by line.
//!
//! CP14.2 used it to prove the trimmed registry resolves like the full one: build this file
//! at the commit before the trim (`git archive a7c24dc | tar -x -C <dir>`, copy this file into
//! `crates/llm/examples/`), build it again here, run both with the same two input files and
//! `diff` the outputs. See `docs/backend/progress/phase-14.md`.
//!
//!   cargo run --release -p hermes_llm --example registry_probe -- providers.txt models.txt out.txt
//!
//! Each input file has one name per line. A line of the output is written only when the pair's
//! answer differs from the answer for a model that is in no catalog.

use hermes_llm::canonical::{
    map_to_canonical_model, maybe_get_canonical_model_with_variant_fallback, CanonicalModelRegistry,
};
use hermes_llm::context_limit::ContextLimitResolver;
use hermes_llm::discovery::parse_models_response;
use hermes_llm::formats::openai::openai_reasoning_effort_for_thinking;
use hermes_llm::model::ModelConfig;
use hermes_llm::thinking::ThinkingEffort;
use std::io::Write;

fn answer(provider: &str, model: &str) -> String {
    let registry = CanonicalModelRegistry::bundled().expect("registry");
    let mut out = String::new();
    let mapped = map_to_canonical_model(provider, model, &registry);
    out.push_str(&format!("map={mapped:?}"));
    if let Some((c, variant)) = maybe_get_canonical_model_with_variant_fallback(provider, model) {
        out.push_str(&format!(
            " canon=[{} | {} | variant={} | ctx={} out={:?} | in={:?} out={:?} | reasoning={:?} efforts={:?} tools={} | modal={:?}]",
            c.id,
            c.name,
            variant,
            c.limit.context,
            c.limit.output,
            c.cost.input,
            c.cost.output,
            c.reasoning,
            c.reasoning_efforts,
            c.tool_call,
            c.modalities.input,
        ));
    }
    out.push_str(&format!(
        " resolve={}",
        ContextLimitResolver::new(provider).resolve_local(model, None)
    ));
    let config = ModelConfig::new(model).with_canonical_limits(provider);
    out.push_str(&format!(
        " cfg=[max={:?} reasoning={:?} vision={:?} is_reasoning={}]",
        config.max_tokens,
        config.reasoning,
        config.supports_vision,
        config.is_reasoning_model()
    ));
    let info = hermes_llm::base::model_info_for_provider_model(provider, model);
    out.push_str(&format!(
        " info=[{:?} {}]",
        info.context_limit, info.reasoning
    ));
    let response = serde_json::json!({"data": [{"id": model}]});
    match parse_models_response(&response, provider) {
        Ok(models) => out.push_str(&format!(" discovered={models:?}")),
        Err(e) => out.push_str(&format!(" discovered=err({e})")),
    }
    if provider == "openai" {
        out.push_str(&format!(" openai_reasoning={}", config.is_openai_reasoning_model()));
        for effort in [
            ThinkingEffort::Off,
            ThinkingEffort::Low,
            ThinkingEffort::Medium,
            ThinkingEffort::High,
            ThinkingEffort::Max,
        ] {
            out.push_str(&format!(
                " eff[{effort:?}]={:?}",
                openai_reasoning_effort_for_thinking(model, effort)
            ));
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let read = |path: &str| -> Vec<String> {
        std::fs::read_to_string(path)
            .expect("input")
            .lines()
            .map(str::to_string)
            .collect()
    };
    let providers = read(&args[1]);
    let models = read(&args[2]);
    let mut out = std::io::BufWriter::new(std::fs::File::create(&args[3]).expect("output"));
    let mut shown = 0usize;
    for provider in &providers {
        let baseline = answer(provider, "zz-no-such-model-zz");
        for model in &models {
            let line = answer(provider, model);
            if line.replace(model, "<M>") != baseline.replace("zz-no-such-model-zz", "<M>") {
                writeln!(out, "{provider}\t{model}\t{line}").unwrap();
                shown += 1;
            }
        }
    }
    eprintln!("{} providers, {} models, {} lines", providers.len(), models.len(), shown);
}
