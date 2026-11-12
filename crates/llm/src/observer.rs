//! Counting model requests where they leave the machine.
//!
//! Not from Goose or Hermes (backend behaviour is ours, AGENTS.md rule 1; `docs/backend/PHASES.md`
//! Phase 2). [`ApiClient`](crate::api_client::ApiClient) tells the *observer* of the current
//! [`RequestContext`] about every HTTP attempt of a POST (one call into `response_post` /
//! `multipart_post` is one attempt, so a retried request shows up once per try, streaming requests
//! included). The context says why the request is sent ([`Purpose`]) and for which session, and
//! carries the observer itself.
//!
//! The context is a tokio *task-local*, set with [`scope`]. Task-locals do **not** cross
//! `tokio::spawn` (think Python's `contextvars`, which a new thread doesn't inherit either): code
//! that spawns a task which makes model requests must start it with [`spawn_in_context`], or the
//! requests are not counted. A POST made outside any context is not observed (and logged once at
//! `warn`, every time at `debug`), so an unlabelled caller shows up as a missing count, never a
//! wrong one. Only clients with a provider id (`ApiClient::with_provider_id`, set by the runtime's
//! `ProviderConfig::client`) are observed: other POSTs (Ollama's `api/show` during model
//! discovery) aren't model requests against a quota.
//!
//! Hot path: [`begin`] clones the context (an `Arc` and a short string) and calls the observer,
//! which must not block or do I/O (the runtime's ledger sends on a channel).

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Why a request is sent. The ledger counts by it; tests read it from the
/// `x-hermes-rs-purpose` header (loopback hosts only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Purpose {
    /// The user's turn: every model call of the agent loop.
    Main,
    /// The background memory/skill review.
    Review,
    /// The skill curator.
    Curator,
    /// The session title.
    Title,
    /// The smart-approval judge.
    Approval,
    /// A compression summary.
    Compress,
    /// A delegated child agent.
    Delegate,
    /// `/btw` side questions.
    Btw,
}

impl Purpose {
    pub const ALL: [Purpose; 8] = [
        Purpose::Main,
        Purpose::Review,
        Purpose::Curator,
        Purpose::Title,
        Purpose::Approval,
        Purpose::Compress,
        Purpose::Delegate,
        Purpose::Btw,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Purpose::Main => "main",
            Purpose::Review => "review",
            Purpose::Curator => "curator",
            Purpose::Title => "title",
            Purpose::Approval => "approval",
            Purpose::Compress => "compress",
            Purpose::Delegate => "delegate",
            Purpose::Btw => "btw",
        }
    }

    pub fn parse(name: &str) -> Option<Purpose> {
        Purpose::ALL.into_iter().find(|p| p.as_str() == name)
    }
}

/// What the observer is told about one request.
#[derive(Debug, Clone, Copy)]
pub struct RequestMeta<'a> {
    /// The provider's id in Settings (`ProviderConfig::id`). Only clients labelled with
    /// `ApiClient::with_provider_id` are observed.
    pub provider: &'a str,
    /// The `model` field of the request body, when it has one.
    pub model: Option<&'a str>,
    pub purpose: Purpose,
    pub session_id: Option<&'a str>,
    pub streaming: bool,
}

/// How a request ended, as far as the HTTP layer can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The server answered with this status (a streamed body may still fail later).
    Status(u16),
    /// No answer: connection error, timeout...
    TransportError,
}

/// Receives every observed request. Both methods run on the request's task, so they must be quick
/// and must not block.
pub trait RequestObserver: Send + Sync {
    /// A request is about to leave.
    fn on_request(&self, meta: &RequestMeta<'_>);
    /// The answer (or the failure to get one) for a request announced by `on_request`.
    fn on_response(&self, _meta: &RequestMeta<'_>, _outcome: Outcome) {}
}

/// The current task's request context.
#[derive(Clone)]
pub struct RequestContext {
    pub purpose: Purpose,
    pub session_id: Option<String>,
    pub observer: Arc<dyn RequestObserver>,
}

tokio::task_local! {
    static CONTEXT: RequestContext;
}

/// Runs `future` with `context` as the current request context.
pub async fn scope<F: Future>(context: RequestContext, future: F) -> F::Output {
    CONTEXT.scope(context, future).await
}

/// The current task's context, if one is set.
pub fn current() -> Option<RequestContext> {
    CONTEXT.try_with(Clone::clone).ok()
}

/// Like `tokio::spawn`, but the new task keeps the current request context (a plain `spawn` loses
/// it; see the module doc).
pub fn spawn_in_context<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match current() {
        Some(context) => tokio::spawn(CONTEXT.scope(context, future)),
        None => tokio::spawn(future),
    }
}

/// An announced request, to be finished with its outcome.
pub(crate) struct Observation {
    context: RequestContext,
    provider: String,
    model: Option<String>,
    streaming: bool,
}

/// Announces a request to the current context's observer; `None` when no context is set, or when
/// the client has no provider id (not a model client: e.g. the Ollama `api/show` POSTs of model
/// discovery, which would otherwise be counted under provider `""` or use up the one warning).
pub(crate) fn begin(provider: Option<&str>, model: Option<&str>, streaming: bool) -> Option<Observation> {
    let provider = provider?;
    let Some(context) = current() else {
        tracing::debug!(provider, "uncounted model request (no request context)");
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "a model request was made outside a request context and is not counted \
                 (spawn tasks with observer::spawn_in_context)"
            );
        }
        return None;
    };
    let observation = Observation {
        context,
        provider: provider.to_string(),
        model: model.map(str::to_string),
        streaming,
    };
    observation.context.observer.on_request(&observation.meta());
    Some(observation)
}

impl Observation {
    fn meta(&self) -> RequestMeta<'_> {
        RequestMeta {
            provider: &self.provider,
            model: self.model.as_deref(),
            purpose: self.context.purpose,
            session_id: self.context.session_id.as_deref(),
            streaming: self.streaming,
        }
    }

    pub(crate) fn finish(self, outcome: Outcome) {
        self.context.observer.on_response(&self.meta(), outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<(String, Purpose, Option<String>, Option<u16>)>>,
    }

    impl RequestObserver for Recorder {
        fn on_request(&self, meta: &RequestMeta<'_>) {
            self.seen.lock().push((
                meta.provider.to_string(),
                meta.purpose,
                meta.session_id.map(str::to_string),
                None,
            ));
        }
        fn on_response(&self, _meta: &RequestMeta<'_>, outcome: Outcome) {
            if let (Some(last), Outcome::Status(code)) = (self.seen.lock().last_mut(), outcome) {
                last.3 = Some(code);
            }
        }
    }

    fn context(recorder: &Arc<Recorder>, purpose: Purpose) -> RequestContext {
        RequestContext {
            purpose,
            session_id: Some("s1".into()),
            observer: recorder.clone(),
        }
    }

    #[test]
    fn purposes_round_trip_through_their_names() {
        for purpose in Purpose::ALL {
            assert_eq!(Purpose::parse(purpose.as_str()), Some(purpose));
        }
        assert_eq!(Purpose::parse("nonsense"), None);
    }

    #[tokio::test]
    async fn a_request_outside_any_context_is_not_observed() {
        assert!(current().is_none());
        assert!(begin(Some("p"), Some("m"), false).is_none());
    }

    #[tokio::test]
    async fn an_unlabelled_client_is_not_observed_even_in_a_context() {
        let recorder = Arc::new(Recorder::default());
        scope(context(&recorder, Purpose::Main), async {
            assert!(begin(None, None, false).is_none());
        })
        .await;
        assert!(recorder.seen.lock().is_empty());
    }

    #[tokio::test]
    async fn the_context_reaches_the_observer_with_its_outcome() {
        let recorder = Arc::new(Recorder::default());
        scope(context(&recorder, Purpose::Review), async {
            let observation = begin(Some("prov"), Some("model-x"), true).unwrap();
            observation.finish(Outcome::Status(429));
        })
        .await;
        assert_eq!(
            *recorder.seen.lock(),
            vec![("prov".to_string(), Purpose::Review, Some("s1".to_string()), Some(429))]
        );
    }

    #[tokio::test]
    async fn a_plain_spawn_loses_the_context_but_spawn_in_context_keeps_it() {
        let recorder = Arc::new(Recorder::default());
        scope(context(&recorder, Purpose::Delegate), async {
            let lost = tokio::spawn(async { current().is_some() }).await.unwrap();
            let kept = spawn_in_context(async { current().map(|c| c.purpose) })
                .await
                .unwrap();
            assert!(!lost, "task-locals do not cross tokio::spawn");
            assert_eq!(kept, Some(Purpose::Delegate));
        })
        .await;
        // Nested scopes: the inner one wins, then the outer is back.
        scope(context(&recorder, Purpose::Main), async {
            scope(context(&recorder, Purpose::Title), async {
                assert_eq!(current().unwrap().purpose, Purpose::Title);
            })
            .await;
            assert_eq!(current().unwrap().purpose, Purpose::Main);
        })
        .await;
    }
}
