//! Connects the GPUI app to the agent runtime. GPUI runs its own executor; the runtime and HTTP
//! client need tokio, so a tokio runtime runs on background threads and the UI talks to it
//! through channels (tokio channels work from any executor).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use gpui::{App, Global};
use hermes_runtime::{ClarifyOutcome, ClarifyRequest, ProcessNotice, Runtime, RuntimeConfig};
use hermes_tools::UserBridge;
use tokio::sync::{mpsc, oneshot};

/// Something a tool needs from the UI.
pub enum UiRequest {
    Clarify {
        session_id: String,
        request: ClarifyRequest,
        reply: oneshot::Sender<ClarifyOutcome>,
    },
    ProcessFinished {
        session_id: String,
        notice: ProcessNotice,
    },
    /// A background review (or `/refine`) of a session finished.
    ReviewFinished {
        session_id: String,
        actions: Vec<String>,
        requested: bool,
    },
}

struct UiBridge {
    requests: mpsc::UnboundedSender<UiRequest>,
}

#[async_trait]
impl UserBridge for UiBridge {
    async fn clarify(&self, session_id: &str, request: ClarifyRequest) -> ClarifyOutcome {
        let (reply, answer) = oneshot::channel();
        let sent = self.requests.send(UiRequest::Clarify {
            session_id: session_id.to_string(),
            request,
            reply,
        });
        if sent.is_err() {
            return ClarifyOutcome::Undelivered;
        }
        // A dropped reply (the session view closed) counts as cancelled.
        answer.await.unwrap_or(ClarifyOutcome::Cancelled)
    }

    fn process_finished(&self, session_id: &str, notice: ProcessNotice) {
        let _ = self.requests.send(UiRequest::ProcessFinished {
            session_id: session_id.to_string(),
            notice,
        });
    }

    fn review_finished(&self, session_id: &str, actions: Vec<String>, requested: bool) {
        let _ = self.requests.send(UiRequest::ReviewFinished {
            session_id: session_id.to_string(),
            actions,
            requested,
        });
    }
}

pub struct Backend {
    pub tokio: tokio::runtime::Runtime,
    pub runtime: Arc<Runtime>,
    pub data_dir: PathBuf,
}

impl Backend {
    pub fn start() -> Result<(Self, mpsc::UnboundedReceiver<UiRequest>)> {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("hermes-runtime")
            .enable_all()
            .build()?;
        let config = RuntimeConfig::for_current_user()?;
        let data_dir = config.data_dir.clone();
        let (requests, receiver) = mpsc::unbounded_channel();
        let runtime = {
            let _guard = tokio.enter();
            Arc::new(Runtime::new(config, Arc::new(UiBridge { requests }))?)
        };
        Ok((
            Self {
                tokio,
                runtime,
                data_dir,
            },
            receiver,
        ))
    }

    /// Runs `future` on the tokio runtime; the returned handle can be awaited from GPUI tasks.
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tokio.spawn(future)
    }
}

pub struct BackendGlobal(pub Arc<Backend>);

impl Global for BackendGlobal {}

pub fn backend(cx: &App) -> Arc<Backend> {
    cx.global::<BackendGlobal>().0.clone()
}
