use crate::base::Provider;
use crate::errors::ProviderError;
use crate::maybe_send::MaybeSend;
use async_trait::async_trait;
use std::future::Future;
use std::time::Duration;

// wasm32 hosts run without a tokio runtime, so back off on the host's timer.
#[cfg(target_arch = "wasm32")]
use gloo_timers::future::sleep;
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::sleep;

pub const DEFAULT_MAX_RETRIES: usize = 3;
pub const DEFAULT_INITIAL_RETRY_INTERVAL_MS: u64 = 1000;
pub const DEFAULT_BACKOFF_MULTIPLIER: f64 = 2.0;
pub const DEFAULT_MAX_RETRY_INTERVAL_MS: u64 = 30_000;

/// The longest Retry-After honored as a sleep (Hermes's `RETRY_AFTER_CAP_S`): 600s covers
/// realistic provider rate-limit windows while rejecting pathological ones (a daily-limit 429
/// with `retry_after_seconds: 19919` would otherwise freeze the turn for hours per retry).
pub const MAX_RETRY_WAIT_SECS: u64 = 600;

#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_retries: usize,
    /// Initial interval between retries in milliseconds
    pub initial_interval_ms: u64,
    /// Multiplier for backoff (exponential)
    pub backoff_multiplier: f64,
    /// Maximum interval between retries in milliseconds
    pub max_interval_ms: u64,
    /// When true, only retry on transient errors (ServerError, NetworkError,
    /// RateLimitExceeded). RequestFailed (4xx client errors) will not be retried.
    pub transient_only: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            initial_interval_ms: DEFAULT_INITIAL_RETRY_INTERVAL_MS,
            backoff_multiplier: DEFAULT_BACKOFF_MULTIPLIER,
            max_interval_ms: DEFAULT_MAX_RETRY_INTERVAL_MS,
            transient_only: false,
        }
    }
}

impl RetryConfig {
    pub fn new(
        max_retries: usize,
        initial_interval_ms: u64,
        backoff_multiplier: f64,
        max_interval_ms: u64,
    ) -> Self {
        Self {
            max_retries,
            initial_interval_ms,
            backoff_multiplier,
            max_interval_ms,
            transient_only: false,
        }
    }

    pub fn transient_only(mut self) -> Self {
        self.transient_only = true;
        self
    }

    pub fn max_retries(&self) -> usize {
        self.max_retries
    }

    pub fn delay_for_attempt(&self, attempt: usize) -> Duration {
        if attempt == 0 {
            return Duration::from_millis(0);
        }

        let exponent = (attempt - 1) as u32;
        let base_delay_ms = (self.initial_interval_ms as f64
            * self.backoff_multiplier.powi(exponent as i32)) as u64;

        let capped_delay_ms = std::cmp::min(base_delay_ms, self.max_interval_ms);

        let jitter_factor_to_avoid_thundering_herd = 0.8 + (rand::random::<f64>() * 0.4);
        let jitter_delay_ms =
            (capped_delay_ms as f64 * jitter_factor_to_avoid_thundering_herd) as u64;

        Duration::from_millis(jitter_delay_ms)
    }
}

/// Substrings marking a `RequestFailed` (4xx) as deterministically permanent:
/// Anthropic rejects signed `thinking`/`redacted_thinking` blocks as immutable
/// once a thinking model's config changes mid-conversation, and the identical
/// payload is rebuilt on every retry — so retrying can never succeed.
const PERMANENT_REQUEST_FAILURE_MARKERS: &[&str] = &[
    "blocks in the latest assistant message cannot be modified",
    "must remain as they were in the original response",
    "Reasoning is mandatory for this endpoint",
];

/// Quota 429s a retry cannot fix within the turn (Hermes's `_USAGE_LIMIT_MESSAGE_TOKENS` and
/// `usage_limit_reached` reason, plus OpenRouter's per-account shape): a daily/monthly request
/// limit is reached and the reset is hours away — retrying sleeps out the Retry-After and the
/// turn's timer runs the whole time, instead of ending with the error the chat shows.
const USAGE_LIMIT_MARKERS: &[&str] = &[
    "usage limit reached",
    "usage limit has been reached",
    "usage_limit_reached",
    "request limit reached",
];

fn is_usage_limit(message: &str) -> bool {
    let lower = message.to_lowercase();
    USAGE_LIMIT_MARKERS.iter().any(|marker| lower.contains(marker))
}

fn is_permanent_request_failure(message: &str) -> bool {
    PERMANENT_REQUEST_FAILURE_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
        || is_thinking_signature_error(message)
}

/// Anthropic models (directly or through OpenRouter) reject replayed thinking blocks whose
/// signature no longer matches; retrying the same request cannot succeed.
fn is_thinking_signature_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("thinking")
        && (lower.contains("signature")
            || lower.contains("cannot be modified")
            || lower.contains("block_binding"))
}

pub fn should_retry(error: &ProviderError, config: &RetryConfig) -> bool {
    match error {
        ProviderError::RateLimitExceeded { details, .. } if is_usage_limit(details) => false,
        ProviderError::RateLimitExceeded { .. }
        | ProviderError::ServerError(_)
        | ProviderError::NetworkError(_) => true,
        ProviderError::RequestFailed(message) if is_permanent_request_failure(message) => false,
        ProviderError::RequestFailed(_) => !config.transient_only,
        _ => false,
    }
}

pub async fn retry_operation<F, Fut, T>(
    config: &RetryConfig,
    operation: F,
) -> Result<T, ProviderError>
where
    F: Fn() -> Fut + MaybeSend,
    Fut: Future<Output = Result<T, ProviderError>> + MaybeSend,
    T: MaybeSend,
{
    let mut attempts = 0;

    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(error) => {
                if should_retry(&error, config) && attempts < config.max_retries {
                    attempts += 1;
                    tracing::warn!(
                        "Request failed, retrying ({}/{}): {:?}",
                        attempts,
                        config.max_retries,
                        error
                    );

                    let delay = match &error {
                        ProviderError::RateLimitExceeded {
                            retry_delay: Some(d),
                            ..
                        } => (*d).min(Duration::from_secs(MAX_RETRY_WAIT_SECS)),
                        _ => config.delay_for_attempt(attempts),
                    };

                    sleep(delay).await;
                    continue;
                }
                return Err(error);
            }
        }
    }
}

/// Trait for retry functionality to keep Provider dyn-compatible.
///
/// All `Provider` implementors get this via the blanket impl below.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ProviderRetry {
    fn retry_config(&self) -> RetryConfig {
        RetryConfig::default()
    }

    async fn with_retry<F, Fut, T>(&self, operation: F) -> Result<T, ProviderError>
    where
        F: Fn() -> Fut + MaybeSend,
        Fut: Future<Output = Result<T, ProviderError>> + MaybeSend,
        T: MaybeSend,
    {
        self.with_retry_config(operation, self.retry_config()).await
    }

    async fn with_retry_config<F, Fut, T>(
        &self,
        operation: F,
        config: RetryConfig,
    ) -> Result<T, ProviderError>
    where
        F: Fn() -> Fut + MaybeSend,
        Fut: Future<Output = Result<T, ProviderError>> + MaybeSend,
        T: MaybeSend;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<P: Provider> ProviderRetry for P {
    fn retry_config(&self) -> RetryConfig {
        Provider::retry_config(self)
    }

    async fn with_retry_config<F, Fut, T>(
        &self,
        operation: F,
        config: RetryConfig,
    ) -> Result<T, ProviderError>
    where
        F: Fn() -> Fut + MaybeSend,
        Fut: Future<Output = Result<T, ProviderError>> + MaybeSend,
        T: MaybeSend,
    {
        let mut attempts = 0;
        let mut auth_retried = false;

        loop {
            return match operation().await {
                Ok(result) => Ok(result),
                Err(error) => {
                    // Auth retry is separate from transient-error retries: we get
                    // at most 1 credential refresh, independent of max_retries.
                    if matches!(error, ProviderError::Authentication(_)) && !auth_retried {
                        auth_retried = true;
                        match self.refresh_credentials().await {
                            Ok(()) => {
                                tracing::warn!(
                                    "Credentials refreshed after auth error, retrying: {:?}",
                                    error
                                );
                                continue;
                            }
                            Err(refresh_err) => {
                                tracing::warn!(
                                    "Credential refresh failed, returning original auth error: {:?}",
                                    refresh_err
                                );
                            }
                        }
                    }

                    if should_retry(&error, &config) && attempts < config.max_retries {
                        attempts += 1;
                        tracing::warn!(
                            "Request failed, retrying ({}/{}): {:?}",
                            attempts,
                            config.max_retries,
                            error
                        );

                        let delay = match &error {
                            ProviderError::RateLimitExceeded {
                                retry_delay: Some(provider_delay),
                                ..
                            } => *provider_delay,
                            _ => config.delay_for_attempt(attempts),
                        };

                        let skip_backoff = std::env::var("GOOSE_PROVIDER_SKIP_BACKOFF")
                            .unwrap_or_default()
                            .parse::<bool>()
                            .unwrap_or(false);

                        if skip_backoff {
                            tracing::info!("Skipping backoff due to GOOSE_PROVIDER_SKIP_BACKOFF");
                        } else {
                            tracing::info!("Backing off for {:?} before retry", delay);
                            sleep(delay).await;
                        }
                        continue;
                    }

                    Err(error)
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_retries_request_failed() {
        let config = RetryConfig::default();
        let error = ProviderError::RequestFailed("Bad request (400): model not found".into());
        assert!(should_retry(&error, &config));
    }

    #[test]
    fn never_retries_permanent_thinking_block_400() {
        let config = RetryConfig::default();
        let error = ProviderError::RequestFailed(
            "Bad request (400): {\"message\":\"messages.3.content.1: `thinking` or \
             `redacted_thinking` blocks in the latest assistant message cannot be \
             modified. These blocks must remain as they were in the original \
             response.\"}"
                .into(),
        );
        assert!(!should_retry(&error, &config));
    }

    #[test]
    fn permanent_request_failure_marker_detection() {
        assert!(is_permanent_request_failure(
            "messages.3.content.1: `thinking` blocks in the latest assistant message \
             cannot be modified"
        ));
        assert!(is_permanent_request_failure(
            "These blocks must remain as they were in the original response."
        ));
        assert!(is_permanent_request_failure(
            "messages.1.content.0: Invalid `signature` in `thinking` block."
        ));
        assert!(!is_permanent_request_failure(
            "Bad request (400): model not found"
        ));
    }

    #[test]
    fn transient_only_skips_request_failed() {
        let config = RetryConfig::default().transient_only();
        let error = ProviderError::RequestFailed("Bad request (400): model not found".into());
        assert!(!should_retry(&error, &config));
    }

    #[test]
    fn transient_only_still_retries_server_error() {
        let config = RetryConfig::default().transient_only();
        assert!(should_retry(
            &ProviderError::ServerError("500 internal".into()),
            &config
        ));
    }

    #[test]
    fn transient_only_still_retries_network_error() {
        let config = RetryConfig::default().transient_only();
        assert!(should_retry(
            &ProviderError::NetworkError("connection refused".into()),
            &config
        ));
    }

    #[test]
    fn transient_only_still_retries_rate_limit() {
        let config = RetryConfig::default().transient_only();
        assert!(should_retry(
            &ProviderError::RateLimitExceeded {
                details: "too many requests".into(),
                retry_delay: None,
            },
            &config
        ));
    }

    #[test]
    fn never_retries_auth_errors() {
        let config = RetryConfig::default();
        assert!(!should_retry(
            &ProviderError::Authentication("invalid key".into()),
            &config
        ));
    }

    #[test]
    fn never_retries_a_daily_request_limit_429() {
        // OpenRouter's per-account daily limit: the reset is hours away, so a retry sleeps out
        // the Retry-After instead of ending the turn with the error the chat shows.
        let config = RetryConfig::default();
        let error = ProviderError::RateLimitExceeded {
            details: "user daily request limit reached (800 of 800). Retry after 19919 seconds."
                .into(),
            retry_delay: Some(Duration::from_secs(19919)),
        };
        assert!(!should_retry(&error, &config));
    }

    #[test]
    fn never_retries_usage_limit_429s() {
        let config = RetryConfig::default();
        for details in [
            "usage limit reached for this model",
            "the usage limit has been reached",
            "user monthly request limit reached (100 of 100)",
            "error code 429: usage_limit_reached",
        ] {
            let error = ProviderError::RateLimitExceeded {
                details: details.into(),
                retry_delay: None,
            };
            assert!(!should_retry(&error, &config), "retried: {details}");
        }
    }

    #[test]
    fn still_retries_a_transient_rate_limit() {
        let config = RetryConfig::default();
        let error = ProviderError::RateLimitExceeded {
            details: "please try again in 1s".into(),
            retry_delay: Some(Duration::from_secs(1)),
        };
        assert!(should_retry(&error, &config));
    }

    #[test]
    fn a_retry_sleeps_at_most_the_retry_wait_cap() {
        // A daily-limit hint past the cap degrades to the cap, not to an hour-long sleep.
        let config = RetryConfig::default();
        let error = ProviderError::RateLimitExceeded {
            details: "too many requests".into(),
            retry_delay: Some(Duration::from_secs(19919)),
        };
        assert!(should_retry(&error, &config));
        let delay = match error {
            ProviderError::RateLimitExceeded { retry_delay, .. } => retry_delay
                .map(|d| d.min(Duration::from_secs(MAX_RETRY_WAIT_SECS)))
                .expect("hint kept"),
            _ => unreachable!(),
        };
        assert_eq!(delay, Duration::from_secs(MAX_RETRY_WAIT_SECS));
    }
}
