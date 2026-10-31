use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn readiness_waits_for_both_adapters_and_empty_composition_is_ready() {
    let status = Arc::new(McpStatus::new(0));
    assert!(status.is_settled());
    let calls = Arc::new(AtomicUsize::new(0));
    status.set_notice_sink({
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
        })
    });
    let mcp = status.begin_batch("mcp");
    let wasm = status.begin_batch("wasm");
    status.extend_configured(2);
    status.record_failed_server("first".into());
    status.record_failed_server("second".into());
    status.mark_settled();
    assert!(
        !status.is_settled(),
        "counts cannot finish an adapter's registration phase"
    );
    drop(mcp);
    assert!(!status.is_settled());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(wasm);
    assert!(status.is_settled());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(status.snapshot().connecting, 0);
}

#[test]
fn closed_generation_rejects_late_observations_batches_and_notices() {
    let old = Arc::new(McpStatus::new(0));
    let pending = old.begin_batch("mcp");
    old.extend_configured(1);
    old.close();
    let revision = old.capability_revision();
    let calls = Arc::new(AtomicUsize::new(0));
    old.set_notice_sink({
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
        })
    });
    old.extend_configured(50);
    old.record_failed_server("late server".into());
    old.record_failure("late tool".into());
    old.record_capability_source("mcp", "late", "blocked", "cancelled");
    old.record_connected(McpServerStatus {
        name: "late".into(),
        server_version: "1".into(),
        protocol_version: "1".into(),
        transport: "stdio".into(),
        tools: 1,
    });
    drop(old.begin_batch("wasm"));
    drop(pending);
    assert!(old.is_settled());
    assert_eq!(old.capability_revision(), revision);
    assert_eq!(old.snapshot().configured, 1);
    assert_eq!(old.snapshot().connected, 0);
    assert!(old.snapshot().failures.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let new = Arc::new(McpStatus::new(0));
    assert!(new.is_settled());
    assert_eq!(new.snapshot().configured, 0);
}

#[test]
fn synchronous_failures_are_not_lost_before_notice_wiring() {
    let status = Arc::new(McpStatus::new(0));
    let batch = status.begin_batch("wasm");
    status.extend_configured(1);
    status.record_failed_server("offline failure".into());
    drop(batch);
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = {
        let calls = calls.clone();
        Arc::new(move |count| {
            calls.fetch_add(count, Ordering::SeqCst);
        })
    };
    status.set_notice_sink(sink.clone());
    status.set_notice_sink(sink);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn abandoned_startup_batch_releases_waiters_without_inventing_failures() {
    let status = Arc::new(McpStatus::new(0));
    let batch = status.begin_batch("mcp");
    status.extend_configured(1);
    drop(batch);
    assert!(status.wait_until_settled(std::time::Duration::ZERO));
    assert!(status.snapshot().failures.is_empty());
}
