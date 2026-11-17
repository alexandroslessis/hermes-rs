//! The gateway's budget, config and staging tests (they ran as `gateway::budget_tests`).

use super::*;

/// A gateway on a temporary data folder, nothing configured.
fn gateway(dir: &std::path::Path) -> Arc<Gateway> {
    let config = hermes_runtime::RuntimeConfig {
        data_dir: dir.to_path_buf(),
        ..hermes_runtime::RuntimeConfig::for_current_user().unwrap()
    };
    let hub = Arc::new(Hub::default());
    let runtime = Arc::new(
        Runtime::new(
            config,
            Arc::new(crate::hub::DesktopBridge { hub: hub.clone() }),
        )
        .unwrap(),
    );
    Arc::new(Gateway::new_for_acp(runtime, hub, dir.to_path_buf()))
}

async fn call(gateway: &Arc<Gateway>, method: &str, params: Value) -> RpcResult {
    let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
    gateway.clone().call(method, params, &outbox).await
}

#[tokio::test]
async fn a_data_folder_without_the_key_runs_requests() {
    use hermes_runtime::budget::{BudgetProfile, for_profile};
    let dir = hermes_testdir::tempdir().unwrap();
    // An existing config that has never heard of budget profiles.
    std::fs::write(
        dir.path().join("desktop-config.json"),
        r#"{"display": {"show_reasoning": false}}"#,
    )
    .unwrap();
    let gateway = gateway(dir.path());
    assert_eq!(
        gateway.runtime.budget(),
        for_profile(BudgetProfile::Requests)
    );
    let answer = call(&gateway, "config.get", json!({"key": "budget.profile"}))
        .await
        .unwrap();
    assert_eq!(answer["value"], "requests");
}

#[tokio::test]
async fn config_set_switches_the_profile_and_refuses_bad_values() {
    use hermes_runtime::budget::{BudgetKnobs, BudgetProfile, for_profile};
    let dir = hermes_testdir::tempdir().unwrap();
    let gateway = gateway(dir.path());
    let set = |key: &str, value: Value| {
        let gateway = gateway.clone();
        let params = json!({"key": key, "value": value});
        async move { call(&gateway, "config.set", params).await }
    };
    set("budget.profile", json!("balanced")).await.unwrap();
    assert_eq!(gateway.runtime.budget(), BudgetKnobs::BALANCED);
    // The delegation status tells the effective width, not balanced's 10.
    set("budget.profile", json!("requests")).await.unwrap();
    let status = call(&gateway, "delegation.status", json!({}))
        .await
        .unwrap();
    assert_eq!(status["max_concurrent_children"], 3);
    set("budget.profile", json!("tokens")).await.unwrap();
    assert_eq!(gateway.runtime.budget(), for_profile(BudgetProfile::Tokens));
    let status = call(&gateway, "delegation.status", json!({}))
        .await
        .unwrap();
    assert_eq!(status["max_concurrent_children"], 10);
    // Per-knob overrides sit on top.
    set("budget.max_iterations", json!(42)).await.unwrap();
    assert_eq!(gateway.runtime.budget().max_iterations, 42);
    assert_eq!(gateway.runtime.budget().profile, BudgetProfile::Tokens);
    // Refused: an unknown profile, and 0 for the caps that would break a turn.
    for (key, value) in [
        ("budget.profile", json!("fast")),
        ("budget.result_size_pct", json!(0)),
        ("budget.length_continuations", json!(0)),
        ("budget.invalid_tool_retries", json!(0)),
        ("budget.nonsense", json!(1)),
    ] {
        let error = set(key, value).await.unwrap_err();
        assert_eq!(error.0, INVALID_PARAMS, "{key}");
    }
    assert_eq!(gateway.runtime.budget().max_iterations, 42);
    assert_eq!(gateway.runtime.budget().result_size_pct, 50);
    // Saved to the config record, so the next start reads it back.
    let saved = std::fs::read_to_string(dir.path().join("desktop-config.json")).unwrap();
    assert!(saved.contains("\"tokens\""), "{saved}");
}

/// Stages a memory write the way a tainted session's `memory` call does.
fn stage_memory(gateway: &Gateway, text: &str) -> String {
    let meta = hermes_tools::staging::Meta {
        session_id: "s1".into(),
        sources: vec!["a web page".into()],
        reason: "this turn read a web page".into(),
    };
    let args = json!({"target": "memory", "operations": [{"action": "add", "content": text}]});
    let staged =
        gateway
            .runtime
            .staging()
            .stage_memory_call(&gateway.runtime.memory(), &args, &meta);
    assert_eq!(staged["success"], true, "{staged}");
    gateway.runtime.staging().list().last().unwrap().id.clone()
}

#[tokio::test]
async fn staging_list_has_a_stable_shape() {
    let dir = hermes_testdir::tempdir().unwrap();
    let gateway = gateway(dir.path());
    let empty = call(&gateway, "staging.list", json!({})).await.unwrap();
    assert_eq!(empty, json!({"items": [], "count": 0}));
    let id = stage_memory(&gateway, "likes tea");
    let listed = call(&gateway, "staging.list", json!({})).await.unwrap();
    assert_eq!(listed["count"], 1);
    let item = &listed["items"][0];
    assert_eq!(item["id"], id);
    assert_eq!(item["kind"], "memory");
    assert_eq!(item["summary"], "add memory");
    assert_eq!(item["status"], "pending");
    assert_eq!(item["session_id"], "s1");
    assert_eq!(item["sources"], json!(["a web page"]));
    assert_eq!(item["reason"], "this turn read a web page");
    assert!(item["diff"].as_str().unwrap().contains("+ likes tea"));
    assert!(item["created"].as_i64().unwrap() > 0);
    assert!(item["error"].is_null());
    // The payload is not part of the answer.
    assert!(item.get("op").is_none());
}

#[tokio::test]
async fn staging_apply_saves_the_write_and_reject_drops_it() {
    let dir = hermes_testdir::tempdir().unwrap();
    let gateway = gateway(dir.path());
    let keep = stage_memory(&gateway, "likes tea");
    let drop = stage_memory(&gateway, "likes coffee");
    let applied = call(&gateway, "staging.apply", json!({"id": keep}))
        .await
        .unwrap();
    assert_eq!(applied["id"], keep);
    assert_eq!(applied["kind"], "memory");
    assert_eq!(applied["applied"], true);
    assert!(applied["message"].is_string());
    assert_eq!(
        gateway
            .runtime
            .memory()
            .entries(hermes_memory::Target::Memory),
        ["likes tea"]
    );
    let rejected = call(&gateway, "staging.reject", json!({"id": drop}))
        .await
        .unwrap();
    assert_eq!(
        rejected,
        json!({"id": drop, "kind": "memory", "rejected": true})
    );
    let listed = call(&gateway, "staging.list", json!({})).await.unwrap();
    assert_eq!(listed["count"], 0);
    assert_eq!(
        gateway
            .runtime
            .memory()
            .entries(hermes_memory::Target::Memory),
        ["likes tea"]
    );
}

#[tokio::test]
async fn staging_errors_say_what_went_wrong() {
    let dir = hermes_testdir::tempdir().unwrap();
    let gateway = gateway(dir.path());
    for method in ["staging.apply", "staging.reject"] {
        let missing = call(&gateway, method, json!({})).await.unwrap_err();
        assert_eq!(missing.0, INVALID_PARAMS, "{method}");
        for id in ["abcdef012345", "../sessions", "", r"..\..\x"] {
            let unknown = call(&gateway, method, json!({"id": id})).await.unwrap_err();
            assert_eq!(unknown.0, SERVER_ERROR, "{method} {id}");
            assert!(unknown.1.starts_with("No staged item"), "{}", unknown.1);
        }
    }
    // A write that can no longer be applied: an error that says why, and the item stays.
    let id = stage_memory(&gateway, "a fairly long entry that no longer fits");
    gateway
        .runtime
        .memory()
        .set_config(hermes_memory::MemoryConfig {
            memory_char_limit: 10,
            ..Default::default()
        });
    let failed = call(&gateway, "staging.apply", json!({"id": id}))
        .await
        .unwrap_err();
    assert_eq!(failed.0, SERVER_ERROR);
    assert!(
        failed.1.starts_with("Could not apply this change:"),
        "{}",
        failed.1
    );
    assert!(
        gateway
            .runtime
            .memory()
            .entries(hermes_memory::Target::Memory)
            .is_empty()
    );
    let listed = call(&gateway, "staging.list", json!({})).await.unwrap();
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["items"][0]["status"], "pending");
    assert!(listed["items"][0]["error"].is_string());
}

#[tokio::test]
async fn staging_over_rest_mirrors_the_methods() {
    let dir = hermes_testdir::tempdir().unwrap();
    let gateway = gateway(dir.path());
    let id = stage_memory(&gateway, "likes tea");
    let (status, body) = gateway
        .rest("GET", "/api/staging", None, b"")
        .await
        .unwrap();
    assert_eq!((status, body["count"].clone()), (200, json!(1)));
    let (status, body) = gateway
        .rest("POST", "/api/staging/abcdef012345/apply", None, b"")
        .await
        .unwrap();
    assert_eq!(status, 404);
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .starts_with("No staged item")
    );
    let (status, body) = gateway
        .rest("POST", &format!("/api/staging/{id}/apply"), None, b"")
        .await
        .unwrap();
    assert_eq!((status, body["applied"].clone()), (200, json!(true)));
    let (status, _) = gateway
        .rest("POST", &format!("/api/staging/{id}/reject"), None, b"")
        .await
        .unwrap();
    assert_eq!(status, 404, "already applied");
    // Not a route: Hermes's 404.
    assert!(
        gateway
            .rest("DELETE", "/api/staging", None, b"")
            .await
            .is_none()
    );
    assert!(
        gateway
            .rest("POST", "/api/staging/x/nope", None, b"")
            .await
            .is_none()
    );
}
