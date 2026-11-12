//! The request observer sees every HTTP attempt: retried ones once per try, streaming ones too,
//! and a call outside a request context not at all.

use std::sync::{Arc, Mutex};

use hermes_llm::api_client::{ApiClient, AuthMethod};
use hermes_llm::base::Provider;
use hermes_llm::conversation::message::Message;
use hermes_llm::model::ModelConfig;
use hermes_llm::observer::{self, Outcome, Purpose, RequestContext, RequestMeta, RequestObserver};
use hermes_llm::openai_compatible::OpenAiCompatibleProvider;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct Recorder {
    /// (provider, model, purpose, session, streaming, status)
    seen: Mutex<Vec<(String, Option<String>, Purpose, Option<String>, bool, Option<u16>)>>,
}

impl RequestObserver for Recorder {
    fn on_request(&self, meta: &RequestMeta<'_>) {
        self.seen.lock().unwrap().push((
            meta.provider.to_string(),
            meta.model.map(str::to_string),
            meta.purpose,
            meta.session_id.map(str::to_string),
            meta.streaming,
            None,
        ));
    }
    fn on_response(&self, _meta: &RequestMeta<'_>, outcome: Outcome) {
        if let (Some(last), Outcome::Status(code)) = (self.seen.lock().unwrap().last_mut(), outcome) {
            last.5 = Some(code);
        }
    }
}

fn ok_stream() -> ResponseTemplate {
    let chunk = json!({"id": "c", "object": "chat.completion.chunk", "model": "test-model",
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}]});
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
}

fn provider_for(server: &MockServer) -> OpenAiCompatibleProvider {
    let client = ApiClient::new_with_tls(
        format!("{}/v1", server.uri()),
        AuthMethod::BearerToken("k".into()),
        None,
    )
    .unwrap()
    .with_provider_id("test");
    OpenAiCompatibleProvider::new("test".into(), client, String::new())
}

async fn ask(provider: &OpenAiCompatibleProvider) -> Result<(), hermes_llm::errors::ProviderError> {
    provider
        .complete(
            &ModelConfig::new("test-model"),
            "system",
            &[Message::user().with_text("hello")],
            &[],
        )
        .await
        .map(|_| ())
}

fn context(recorder: &Arc<Recorder>, purpose: Purpose) -> RequestContext {
    RequestContext {
        purpose,
        session_id: Some("s1".into()),
        observer: recorder.clone(),
    }
}

#[tokio::test]
async fn a_streaming_request_is_observed_once_with_its_labels() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok_stream())
        .mount(&server)
        .await;
    let recorder = Arc::new(Recorder::default());
    observer::scope(context(&recorder, Purpose::Review), ask(&provider_for(&server)))
        .await
        .unwrap();
    assert_eq!(
        *recorder.seen.lock().unwrap(),
        vec![(
            "test".to_string(),
            Some("test-model".to_string()),
            Purpose::Review,
            Some("s1".to_string()),
            true,
            Some(200)
        )]
    );
}

#[tokio::test]
async fn every_retry_attempt_is_observed() {
    // No backoff sleeps between the attempts (read by the retry layer).
    unsafe { std::env::set_var("GOOSE_PROVIDER_SKIP_BACKOFF", "true") };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": {"message": "boom"}})))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok_stream())
        .with_priority(2)
        .mount(&server)
        .await;
    let recorder = Arc::new(Recorder::default());
    observer::scope(context(&recorder, Purpose::Main), ask(&provider_for(&server)))
        .await
        .unwrap();
    let statuses: Vec<Option<u16>> = recorder.seen.lock().unwrap().iter().map(|s| s.5).collect();
    assert_eq!(statuses, vec![Some(500), Some(500), Some(200)]);
    // The server saw exactly the attempts the observer counted.
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn a_request_outside_a_context_still_works_and_is_not_counted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok_stream())
        .mount(&server)
        .await;
    let recorder = Arc::new(Recorder::default());
    ask(&provider_for(&server)).await.unwrap();
    // A plain spawn inside a scope loses the context; spawn_in_context keeps it.
    let provider = Arc::new(provider_for(&server));
    observer::scope(context(&recorder, Purpose::Delegate), async {
        let lost = provider.clone();
        tokio::spawn(async move { ask(&lost).await.unwrap() }).await.unwrap();
        let kept = provider.clone();
        observer::spawn_in_context(async move { ask(&kept).await.unwrap() })
            .await
            .unwrap();
    })
    .await;
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    let seen = recorder.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "only the request that kept its context");
    assert_eq!(seen[0].2, Purpose::Delegate);
}
