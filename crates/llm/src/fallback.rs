//! Which provider failures send a request to the next provider of the fallback chain.
//!
//! Not from Hermes (backend behaviour is ours, AGENTS.md rule 1; `docs/backend/PHASES.md`
//! Phase 12; Hermes's own chain is `agent/agent_init_fallback.py`, which switches on far more
//! failures). A switch sends the conversation to ANOTHER provider, so the list is short and each
//! entry is about the provider being unable to serve, never about the request being wrong:
//!
//! | failure | switch? |
//! |---|---|
//! | a daily/monthly quota 429 (`(800 of 800)`, "free-models-per-day", "usage limit reached", a Retry-After of an hour or more) | yes: it resets hours away |
//! | 5xx, or no answer (timeout, connection refused), after the provider layer's own retries | yes |
//! | model not found (404 or 400 saying the model is unknown / invalid / does not exist) | yes |
//! | a per-minute 429 (Retry-After of seconds): the existing back-off already waited | no |
//! | 401 / 403: a wrong key is the user's to fix; switching would hide it | no |
//! | 400, 422 and any other 4xx: the request is wrong, it fails anywhere | no |
//! | context too long (413, 400 "maximum context"): compression handles it | no |
//! | 402 credits exhausted, content refusals, stream decode errors | no |
//!
//! [`classify`] is the only place that decides; the agent's `InferenceRunner` asks it.

use std::time::Duration;

use crate::errors::ProviderError;

/// A Retry-After of at least this long is not a per-minute rate limit but a quota window
/// (`extract_retry_after` clamps every hint to one hour, so a quota hint reads as exactly this).
const QUOTA_RETRY_AFTER: Duration = Duration::from_secs(3600);

/// Why the request moves to the next provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The provider's daily (or longer) request quota is used up.
    DailyQuota,
    /// Our own governor says the provider's configured daily limit is used up; no request was sent.
    GovernorExhausted,
    /// A 5xx that outlasted the retries.
    ServerError,
    /// Timeout, refused connection: the provider did not answer, retries included.
    Unreachable,
    /// The provider does not serve the configured model.
    ModelNotFound,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::DailyQuota => "daily_quota",
            Trigger::GovernorExhausted => "governor_exhausted",
            Trigger::ServerError => "server_error",
            Trigger::Unreachable => "unreachable",
            Trigger::ModelNotFound => "model_not_found",
        }
    }

    /// The reason as the notice words it: "`<provider>` <this>".
    pub fn describe(self) -> &'static str {
        match self {
            Trigger::DailyQuota => "has used up its request quota",
            Trigger::GovernorExhausted => "has used up its daily request limit",
            Trigger::ServerError => "kept answering with server errors",
            Trigger::Unreachable => "did not answer",
            Trigger::ModelNotFound => "does not serve the model",
        }
    }

    /// Whether the provider is out of requests until its reset (the next turn need not try it).
    pub fn is_quota(self) -> bool {
        matches!(self, Trigger::DailyQuota | Trigger::GovernorExhausted)
    }
}

/// `Some(trigger)` when `error` (the final one, after the provider layer's retries) sends the
/// request to the next provider of the chain; `None` for every other failure.
pub fn classify(error: &ProviderError) -> Option<Trigger> {
    match error {
        ProviderError::RateLimitExceeded {
            details,
            retry_delay,
        } => is_daily_quota(details, *retry_delay).then_some(Trigger::DailyQuota),
        ProviderError::ServerError(_) => Some(Trigger::ServerError),
        // A stream that broke while decoding is a protocol problem, not an outage.
        ProviderError::NetworkError(message) if message.starts_with("Stream decode error") => None,
        ProviderError::NetworkError(_) => Some(Trigger::Unreachable),
        ProviderError::RequestFailed(message) | ProviderError::EndpointNotFound(message) => {
            is_model_not_found(message).then_some(Trigger::ModelNotFound)
        }
        // Auth (401/403), context length, credits (402), refusals, bad values...: not ours to
        // route around.
        _ => None,
    }
}

/// A 429 that says the quota (not the rate) is gone: the provider layer's usage-limit markers,
/// an "N of N" count, or a Retry-After of an hour or more.
pub fn is_daily_quota(details: &str, retry_delay: Option<Duration>) -> bool {
    crate::retry::is_usage_limit(details)
        || daily_limit_in(details).is_some()
        || retry_delay.is_some_and(|delay| delay >= QUOTA_RETRY_AFTER)
}

/// Phrases the providers use for an unknown model (OpenAI `model_not_found`, Ollama `model "x"
/// not found, try pulling it first`, OpenRouter `x is not a valid model ID` and `No endpoints
/// found for x`, llama.cpp / LM Studio `model ... does not exist`).
pub fn is_model_not_found(message: &str) -> bool {
    let lower = body_of(message).to_lowercase();
    lower.contains("model_not_found")
        || lower.contains("not a valid model")
        || lower.contains("invalid model")
        || lower.contains("unknown model")
        || lower.contains("no endpoints found")
        || model_then(&lower, "not found")
        || model_then(&lower, "does not exist")
}

/// "model" followed closely by `phrase` in the same sentence ("model \"x\" not found", "The model `x`
/// does not exist"): the subject of the phrase is the model, not something else a long 400
/// mentions beside a model ("tool call id not found ... model output" must not switch).
fn model_then(lower: &str, phrase: &str) -> bool {
    /// Room for a long model id between the two ("hf.co/org/Some-Model-GGUF:Q4_K_M").
    const GAP: usize = 120;
    lower.match_indices("model").any(|(at, _)| {
        let after = &lower[at + "model".len()..];
        after.find(phrase).is_some_and(|gap| {
            // Within one sentence: a model id has dots, but not a dot and a space.
            let between = &after[..gap];
            gap <= GAP && !between.contains('\n') && !between.contains(". ")
        })
    })
}

/// The provider's own words in an error message: the provider layer prefixes a 404 with the
/// request URL ("Resource not found (404) at https://models.example/v1/...: <body>"), and a host
/// named `models.*` must not read as "model not found".
fn body_of(message: &str) -> &str {
    match message.find(" at http") {
        Some(at) => message[at..]
            .find(": ")
            .map_or("", |colon| &message[at + colon + 2..]),
        None => message,
    }
}

/// The daily limit a provider's 429 message states as "N of N" ("...daily request limit reached
/// (800 of 800)"): `Some(N)` when both numbers are the same, `None` otherwise.
pub fn daily_limit_in(message: &str) -> Option<u32> {
    // Thousands separators ("1,000 of 1,000") are dropped first, so the numbers stay whole.
    let chars: Vec<char> = message.chars().collect();
    let message: String = chars
        .iter()
        .enumerate()
        .filter(|&(i, &c)| {
            !(c == ','
                && i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        })
        .map(|(_, &c)| c)
        .collect();
    let words: Vec<&str> = message
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '.' | ';' | ':'))
        .filter(|word| !word.is_empty())
        .collect();
    words.windows(3).find_map(|w| {
        let (used, limit) = (w[0].parse::<u32>().ok()?, w[2].parse::<u32>().ok()?);
        (w[1].eq_ignore_ascii_case("of") && used == limit && limit > 0).then_some(limit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_status::map_http_error;
    use reqwest::StatusCode;
    use serde_json::json;

    /// The error the provider layer builds for a response, as the runner sees it (a 429's
    /// Retry-After is folded in and clamped to an hour, as `extract_retry_after` does).
    fn from_response(
        status: u16,
        body: serde_json::Value,
        retry_after: Option<u64>,
    ) -> ProviderError {
        let error = map_http_error(
            StatusCode::from_u16(status).unwrap(),
            Some(body),
            "https://provider.test/v1/chat/completions",
            None,
        );
        match (error, retry_after) {
            (ProviderError::RateLimitExceeded { details, .. }, Some(secs)) => {
                ProviderError::RateLimitExceeded {
                    details,
                    retry_delay: Some(Duration::from_secs(secs.min(3600))),
                }
            }
            (error, _) => error,
        }
    }

    #[test]
    fn openrouters_daily_limit_429_switches() {
        // The user's provider: ~800 requests a day, the body names the count.
        let error = from_response(
            429,
            json!({"error": {
                "message": "Rate limit exceeded: user daily request limit reached (800 of 800). Retry after 19919 seconds.",
                "code": 429,
                "metadata": {"retry_after_seconds": 19919}
            }}),
            Some(19919),
        );
        assert_eq!(classify(&error), Some(Trigger::DailyQuota));
    }

    #[test]
    fn openrouters_free_tier_daily_limit_switches() {
        let error = from_response(
            429,
            json!({"error": {
                "message": "Rate limit exceeded: free-models-per-day. Add 10 credits to unlock 1000 free model requests per day",
                "code": 429,
                "metadata": {"headers": {"X-RateLimit-Limit": "50", "X-RateLimit-Remaining": "0", "X-RateLimit-Reset": "1760000000000"}}
            }}),
            None,
        );
        assert_eq!(classify(&error), Some(Trigger::DailyQuota));
    }

    #[test]
    fn other_daily_quota_wordings_switch() {
        for message in [
            "Rate limit reached for model on tokens per day (TPD): Limit 500000, Used 499990",
            "You have used 50 of 50 requests",
            "usage limit reached for this plan",
            "error code: usage_limit_reached",
            "The daily limit for this key has been exceeded",
        ] {
            let error = from_response(429, json!({"error": {"message": message}}), None);
            assert_eq!(classify(&error), Some(Trigger::DailyQuota), "{message}");
        }
        // No marker at all, but the provider says to come back in hours.
        let error = from_response(
            429,
            json!({"error": {"message": "Too many requests"}}),
            Some(18000),
        );
        assert_eq!(classify(&error), Some(Trigger::DailyQuota));
    }

    #[test]
    fn a_per_minute_429_does_not_switch() {
        // Seconds of Retry-After: the back-off already waited and the same provider is fine.
        for (message, retry) in [
            (
                "Rate limit exceeded: limit_rpm/model/ab12. Please try again in 8s",
                Some(8),
            ),
            ("Too many requests", Some(30)),
            ("Rate limit reached for requests per minute", Some(2)),
            ("slow down", None),
        ] {
            let error = from_response(429, json!({"error": {"message": message}}), retry);
            assert_eq!(classify(&error), None, "{message}");
        }
        // The partial count of a per-minute window is not "N of N".
        let error = from_response(
            429,
            json!({"error": {"message": "(790 of 800) in the last minute"}}),
            Some(5),
        );
        assert_eq!(classify(&error), None);
    }

    #[test]
    fn auth_and_ordinary_4xx_never_switch() {
        for (status, body) in [
            (401, json!({"error": {"message": "Invalid API key"}})),
            (
                403,
                json!({"error": {"message": "Forbidden: key lacks access"}}),
            ),
            (
                400,
                json!({"error": {"message": "messages: field required"}}),
            ),
            (
                400,
                json!({"error": {"message": "Invalid parameter: tools[3].function.parameters"}}),
            ),
            (
                400,
                json!({"error": {"message": "response_format json_schema is not supported"}}),
            ),
            (422, json!({"detail": [{"msg": "field required: body"}]})),
            (
                422,
                json!({"error": {"message": "temperature must be below 2"}}),
            ),
            (402, json!({"error": {"message": "Insufficient credits"}})),
            (404, json!({"error": {"message": "Not Found"}})),
        ] {
            let error = from_response(status, body.clone(), None);
            assert_eq!(classify(&error), None, "{status} {body}");
        }
    }

    #[test]
    fn context_too_long_never_switches() {
        for (status, message) in [
            (413, "Request entity too large"),
            (
                400,
                "This model's maximum context length is 8192 tokens. However, you requested 9000 tokens",
            ),
        ] {
            let error = from_response(status, json!({"error": {"message": message}}), None);
            assert!(
                matches!(error, ProviderError::ContextLengthExceeded(_)),
                "{message}"
            );
            assert_eq!(classify(&error), None);
        }
    }

    #[test]
    fn server_errors_and_no_answer_switch() {
        for status in [500, 502, 503, 504, 529] {
            let error = from_response(
                status,
                json!({"error": {"message": "upstream unavailable"}}),
                None,
            );
            assert_eq!(classify(&error), Some(Trigger::ServerError), "{status}");
        }
        for message in [
            "Request timed out — check your network connection and try again.",
            "Could not connect to localhost:11434 — check your network connection and try again.",
        ] {
            assert_eq!(
                classify(&ProviderError::NetworkError(message.into())),
                Some(Trigger::Unreachable)
            );
        }
        // A stream that stopped decoding is a protocol problem, not an outage.
        assert_eq!(
            classify(&ProviderError::NetworkError(
                "Stream decode error: bad chunk".into()
            )),
            None
        );
    }

    #[test]
    fn an_unknown_model_switches() {
        for (status, body) in [
            (
                404,
                json!({"error": {"message": "The model `gpt-9` does not exist or you do not have access to it.", "code": "model_not_found"}}),
            ),
            (
                404,
                json!({"error": {"message": "model \"llama9\" not found, try pulling it first"}}),
            ),
            (
                404,
                json!({"error": {"message": "No endpoints found for anthropic/claude-9.", "code": 404}}),
            ),
            (
                400,
                json!({"error": {"message": "acme/model-x is not a valid model ID", "code": 400}}),
            ),
            (
                400,
                json!({"error": {"message": "Invalid model: gpt-nope"}}),
            ),
            (404, json!({"error": {"message": "Model not found"}})),
        ] {
            let error = from_response(status, body.clone(), None);
            assert_eq!(
                classify(&error),
                Some(Trigger::ModelNotFound),
                "{status} {body}"
            );
        }
    }

    #[test]
    fn a_400_that_mentions_a_model_and_something_not_found_does_not_switch() {
        for message in [
            "Tool call id call_1 not found in the messages sent to the model",
            "messages.2: tool_result block not found; the model expects one after each tool_use. File not found.",
        ] {
            let error = from_response(400, json!({"error": {"message": message}}), None);
            assert_eq!(classify(&error), None, "{message}");
        }
    }

    #[test]
    fn a_models_host_name_is_not_a_missing_model() {
        // The 404 text names the URL; only the provider's own words count.
        let error = ProviderError::RequestFailed(
            "Resource not found (404) at https://models.example.ai/inference/chat/completions: Not Found".into(),
        );
        assert_eq!(classify(&error), None);
        let error = ProviderError::RequestFailed(
            "Resource not found (404) at https://models.example.ai/inference/chat/completions: model foo not found".into(),
        );
        assert_eq!(classify(&error), Some(Trigger::ModelNotFound));
    }

    #[test]
    fn other_errors_do_not_switch() {
        for error in [
            ProviderError::NotConfigured,
            ProviderError::Authentication("bad key".into()),
            ProviderError::InvalidValue("x".into()),
            ProviderError::ExecutionError("boom".into()),
            ProviderError::CreditsExhausted {
                details: "empty".into(),
                top_up_url: None,
            },
            ProviderError::Refusal {
                details: "policy".into(),
                category: None,
            },
            ProviderError::RequestFailed("Request failed with status 418 at u: teapot".into()),
        ] {
            assert_eq!(classify(&error), None, "{error}");
        }
    }

    #[test]
    fn n_of_n_needs_equal_numbers() {
        assert_eq!(
            daily_limit_in("daily request limit reached (800 of 800)."),
            Some(800)
        );
        assert_eq!(daily_limit_in("(790 of 800)"), None);
        assert_eq!(daily_limit_in("0 of 0"), None);
        assert_eq!(
            daily_limit_in("limit reached (1,000 of 1,000), retry later"),
            Some(1000)
        );
    }
}
