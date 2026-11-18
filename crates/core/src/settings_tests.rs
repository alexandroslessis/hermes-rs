//! Settings, usage, the curator and staging over a real runtime on a temporary data folder, with
//! no gateway (no model is called).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hermes_runtime::{ProviderConfig, RuntimeConfig};
use serde_json::json;

use crate::events::CoreEvent;
use crate::providers::Settings;
use crate::settings::{ModelSwitch, setup_status};
use crate::{Core, NewSession};

struct Fixture {
    core: Arc<Core>,
    data: std::path::PathBuf,
    work: hermes_testdir::TestDir,
    // Last: the runtime above holds files in it until it drops.
    _data: hermes_testdir::TestDir,
}

fn fixture(config_json: &str) -> Fixture {
    let data = hermes_testdir::tempdir().unwrap();
    std::fs::write(data.path().join("config.json"), config_json).unwrap();
    let config = RuntimeConfig {
        data_dir: data.path().to_path_buf(),
        ..RuntimeConfig::for_current_user().unwrap()
    };
    let core = Core::new(config, tokio::runtime::Handle::current()).unwrap();
    Fixture {
        core,
        data: data.path().to_path_buf(),
        work: hermes_testdir::tempdir().unwrap(),
        _data: data,
    }
}

const TWO_LOCAL: &str = r#"{"providers": [
    {"id": "a", "name": "A", "base_url": "http://127.0.0.1:1/v1"},
    {"id": "b", "name": "B", "base_url": "http://127.0.0.1:2/v1"}],
  "provider": "a", "model": "m1"}"#;

fn session_on(f: &Fixture, provider: &str, model: &str) -> String {
    let (record, _) = f
        .core
        .create_session(NewSession {
            provider: ProviderConfig {
                id: provider.into(),
                base_url: "http://127.0.0.1:1/v1".into(),
                api_key: None,
            },
            model: model.into(),
            cwd: f.work.path().to_path_buf(),
            title: None,
            seeded: vec![],
            parent: None,
        })
        .unwrap();
    record.id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_config_record_reaches_the_runtime_with_no_gateway() {
    let f = fixture(TWO_LOCAL);
    // Manual approvals by default; the record's choice applies when the core applies Settings.
    f.core.apply_settings();
    assert!(f.core.runtime().approvals().asks());
    assert!(!f.core.runtime().approvals().smart());
    f.core.config().set_value("approvals.mode", json!("smart"));
    f.core.config().set_value("budget.profile", json!("tokens"));
    f.core.apply_settings();
    assert!(f.core.runtime().approvals().smart());
    assert_eq!(
        f.core.runtime().budget().profile,
        hermes_runtime::budget::BudgetProfile::Tokens
    );
    // It is the record the file holds: a second core on the folder reads the same.
    assert_eq!(
        crate::config::ConfigRecord::load(&f.data).value("approvals.mode"),
        "smart"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_pick_moves_the_session_and_the_default() {
    let f = fixture(TWO_LOCAL);
    let id = session_on(&f, "a", "m1");
    let settings = Settings::load(&f.data);
    let announced = AtomicUsize::new(0);
    let changed = |session: &str| {
        assert_eq!(session, id);
        announced.fetch_add(1, Ordering::Relaxed);
    };
    // A session-only pick: the session moves, the default stays.
    let switched = f
        .core
        .switch_model(
            &ModelSwitch {
                model: "m2".into(),
                provider: Some("a".into()),
                session_only: true,
                session_id: Some(id.clone()),
            },
            &settings,
            &changed,
        )
        .unwrap();
    assert_eq!(
        (switched.model.as_str(), switched.provider.as_str()),
        ("m2", "a")
    );
    assert_eq!(announced.load(Ordering::Relaxed), 1);
    assert_eq!(
        f.core
            .runtime()
            .store()
            .session(&id)
            .unwrap()
            .unwrap()
            .model,
        "m2"
    );
    assert_eq!(Settings::load(&f.data).model.as_deref(), Some("m1"));
    // Without `session_only` the default changes too.
    f.core
        .switch_model(
            &ModelSwitch {
                model: "m3".into(),
                provider: Some("b".into()),
                session_only: false,
                session_id: None,
            },
            &settings,
            &changed,
        )
        .unwrap();
    let after = Settings::load(&f.data);
    assert_eq!(
        (after.provider.as_deref(), after.model.as_deref()),
        (Some("b"), Some("m3"))
    );
    // An unknown provider is refused and changes nothing.
    let refused = f.core.switch_model(
        &ModelSwitch {
            model: "x".into(),
            provider: Some("nobody".into()),
            session_only: false,
            session_id: None,
        },
        &settings,
        &changed,
    );
    assert_eq!(refused.unwrap_err(), "Unknown provider nobody");
    assert_eq!(Settings::load(&f.data).model.as_deref(), Some("m3"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_finds_its_provider_and_a_turn_its_options() {
    let f = fixture(TWO_LOCAL);
    let id = session_on(&f, "b", "m9");
    let settings = Settings::load(&f.data);
    let provider = f.core.provider_for(&id, &settings).unwrap();
    assert_eq!(provider.base_url, "http://127.0.0.1:2/v1");
    // The session's own provider is gone from Settings: the default one stands in.
    let other = session_on(&f, "gone", "m");
    let provider = f.core.session_provider(&other, &settings).unwrap();
    assert_eq!(provider.base_url, "http://127.0.0.1:1/v1");
    assert_eq!(
        f.core
            .provider_for("nope", &settings)
            .unwrap_err()
            .to_string(),
        "Session nope not found"
    );
    // The endpoint's window is the turn's window; no picked effort sends none.
    std::fs::write(
        f.data.join("config.json"),
        r#"{"providers": [{"id": "b", "name": "B", "base_url": "http://127.0.0.1:2/v1",
                            "context_length": 32768}], "provider": "b", "model": "m9"}"#,
    )
    .unwrap();
    let options = f.core.turn_options(&id);
    assert_eq!(options.context_length, Some(32768));
    assert_eq!(options.effort, None);
    f.core.pick_effort(&id, Some("ultra".into()));
    assert_eq!(f.core.turn_options(&id).effort.as_deref(), Some("max"));
    assert_eq!(f.core.effort(&id), "ultra");
    // The import route is the default endpoint and its model.
    let (route, model) = f.core.import_route(&Settings::load(&f.data)).unwrap();
    assert_eq!((route.id.as_str(), model.as_str()), ("b", "m9"));
    assert!(f.core.import_route(&Settings::default()).is_err());
    assert!(setup_status(&Settings::load(&f.data)).ready);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_counts_generation_speed_and_the_quota() {
    let f = fixture(TWO_LOCAL);
    let id = session_on(&f, "a", "m1");
    let fresh = f.core.usage_snapshot(&id, None);
    assert_eq!(fresh.model.as_deref(), Some("m1"));
    assert_eq!(fresh.totals.calls, 0);
    assert!(fresh.avg_tps.is_none() && fresh.context.is_none() && fresh.quota.is_none());
    // 100 tokens in 4 s, then 100 in 1 s: 40 tokens per second over the session.
    f.core.note_generation(&id, 100, 4.0);
    f.core.note_generation(&id, 100, 1.0);
    f.core.note_generation(&id, 0, 1.0);
    let snapshot = f.core.usage_snapshot(&id, Some(1000));
    assert_eq!(snapshot.avg_tps, Some(40.0));
    // No window known yet: the display fallback, and the context is shown from the call's input.
    let context = snapshot.context.unwrap();
    assert_eq!(
        (context.used, context.max),
        (1000, crate::settings::FALLBACK_CONTEXT)
    );
    // The speed is kept beside the store.
    assert!(f.data.join(crate::usage::GENERATION_FILE).exists());
    // A daily limit shows as the gauge.
    std::fs::write(
        f.data.join("config.json"),
        r#"{"providers": [{"id": "a", "name": "A", "base_url": "http://127.0.0.1:1/v1",
                            "daily_request_limit": 800}], "provider": "a", "model": "m1"}"#,
    )
    .unwrap();
    f.core.apply_settings();
    let quota = f.core.usage_snapshot(&id, None).quota.unwrap();
    assert_eq!((quota.used, quota.limit, quota.used_bp), (0, 800, 0));
    assert_eq!(f.core.requests_status("a").limit, Some(800));
    let insights = f.core.insights(30);
    assert_eq!(insights.sessions, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_curator_reports_its_run_as_events_and_one_runs_at_a_time() {
    let f = fixture(TWO_LOCAL);
    let mut events = f.core.subscribe();
    assert!(f.core.run_curator());
    // A second run while one is going is refused.
    assert!(!f.core.run_curator());
    let mut started = false;
    let finished = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            match events.recv().await.unwrap() {
                CoreEvent::CuratorStarted => started = true,
                CoreEvent::CuratorFinished { ok, lines } => break (ok, lines),
                _ => {}
            }
        }
    })
    .await
    .expect("the curator finishes");
    assert!(started);
    let (ok, _lines) = finished;
    let status = f.core.actions().status(crate::maintenance::CURATOR_RUN, 50);
    assert!(!status.running);
    assert_eq!(status.exit_code, Some(if ok { 0 } else { 1 }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_puts_the_stored_settings_on_the_runtime() {
    // A record written by an earlier run: `Core::new` applies none of it, `start` all of it.
    let data = hermes_testdir::tempdir().unwrap();
    std::fs::write(data.path().join("config.json"), TWO_LOCAL).unwrap();
    std::fs::write(
        data.path().join("desktop-config.json"),
        r#"{"approvals": {"mode": "off"}, "checkpoints": {"enabled": false}}"#,
    )
    .unwrap();
    let config = RuntimeConfig {
        data_dir: data.path().to_path_buf(),
        ..RuntimeConfig::for_current_user().unwrap()
    };
    let core = Core::new(config, tokio::runtime::Handle::current()).unwrap();
    assert!(core.runtime().approvals().asks());
    let starter = core.clone();
    tokio::task::spawn_blocking(move || starter.start(false))
        .await
        .unwrap();
    assert!(!core.runtime().approvals().asks());
    assert!(!core.runtime().checkpoints().enabled());
    drop(core);
}

/// Stages a memory write the way a tainted session's `memory` call does.
fn stage_memory(f: &Fixture, text: &str) -> String {
    let runtime = f.core.runtime();
    let meta = hermes_tools::staging::Meta {
        session_id: "s1".into(),
        sources: vec!["a web page".into()],
        reason: "this turn read a web page".into(),
    };
    let args = json!({"target": "memory", "operations": [{"action": "add", "content": text}]});
    let staged = runtime
        .staging()
        .stage_memory_call(&runtime.memory(), &args, &meta);
    assert_eq!(staged["success"], true, "{staged}");
    runtime.staging().list().last().unwrap().id.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_staged_write_is_applied_or_rejected() {
    let f = fixture(TWO_LOCAL);
    let runtime = f.core.runtime();
    let keep = stage_memory(&f, "likes tea");
    let drop_id = stage_memory(&f, "likes coffee");
    let listed = crate::staging::list(runtime).await;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].kind, "memory");
    assert_eq!(listed[0].status, "pending");
    assert!(listed[0].diff.contains("+ likes tea"));
    let applied = crate::staging::apply(runtime, &keep).await.unwrap();
    assert_eq!(
        (applied.id.as_str(), applied.kind),
        (keep.as_str(), "memory")
    );
    assert_eq!(
        runtime.memory().entries(hermes_memory::Target::Memory),
        ["likes tea"]
    );
    let rejected = crate::staging::reject(runtime, &drop_id).await.unwrap();
    assert_eq!(rejected.id, drop_id);
    assert!(crate::staging::list(runtime).await.is_empty());
    // The rejected write was not saved, and neither id can be decided again.
    assert_eq!(
        runtime.memory().entries(hermes_memory::Target::Memory),
        ["likes tea"]
    );
    for id in [&keep, &drop_id] {
        assert!(matches!(
            crate::staging::apply(runtime, id).await,
            Err(crate::staging::Failure::Missing(_))
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_staged_and_an_unknown_item_is_missing() {
    let f = fixture(TWO_LOCAL);
    assert!(crate::staging::list(f.core.runtime()).await.is_empty());
    let missing = crate::staging::apply(f.core.runtime(), "nope")
        .await
        .unwrap_err();
    assert!(matches!(missing, crate::staging::Failure::Missing(_)));
    assert!(matches!(
        crate::staging::reject(f.core.runtime(), "nope").await,
        Err(crate::staging::Failure::Missing(_))
    ));
}
