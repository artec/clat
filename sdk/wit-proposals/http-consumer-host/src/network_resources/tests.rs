use super::*;
use crate::dns_authority::{Fence, Run};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
pub(super) fn scope() -> (Run, Scope) {
    let run = Run::new(std::time::Duration::from_secs(2)).unwrap();
    let scope = Scope::new(
        &run,
        Fence::new(Some(&["https://example.com"]), None).unwrap(),
    );
    (run, scope)
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
struct Probe(Arc<AtomicUsize>);
impl Drop for Probe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn resource_owner_counts_actual_subscriptions_and_cleans_children_first() {
    let (_run, scope) = scope();
    let mut owner = Owner::new(&scope);
    let task = owner
        .insert(Task::new(
            &scope,
            Instant::now() + std::time::Duration::from_secs(1),
            std::future::pending(),
        ))
        .unwrap();
    let mut children = vec![];
    for _ in 0..15 {
        children.push(owner.subscribe(&task).unwrap());
    }
    assert_eq!(owner.len(), 16);
    assert!(matches!(owner.subscribe(&task), Err(Error::Limit)));
    owner.remove(&task).unwrap();
    assert_eq!(owner.len(), 0);
    assert!(owner.table.is_empty());
    assert!(matches!(
        owner.get_mut(&children[0]),
        Err(Error::InvalidHandle)
    ));
}
#[test]
fn resource_owner_drop_and_explicit_close_release_unconsumed_values_once() {
    for close in [false, true] {
        let (_run, scope) = scope();
        let n = Arc::new(AtomicUsize::new(0));
        let mut owner = Owner::new(&scope);
        owner.insert(Probe(n.clone())).unwrap();
        if close {
            owner.close().unwrap();
            owner.close().unwrap();
            assert_eq!(n.load(Ordering::SeqCst), 1);
        }
        drop(owner);
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn resource_owner_foreign_and_recycled_handles_cannot_read_or_delete() {
    let (_run, scope) = scope();
    let mut a = Owner::new(&scope);
    let mut b = Owner::new(&scope);
    let old = a.insert(1u32).unwrap();
    let foreign = b.insert(2u32).unwrap();
    assert!(matches!(b.get_mut(&old), Err(Error::InvalidHandle)));
    assert!(matches!(a.remove(&foreign), Err(Error::InvalidHandle)));
    let forged_owner = Handle::<u32> {
        rep: foreign.rep,
        serial: foreign.serial,
        owner: a.identity,
        marker: PhantomData,
    };
    assert!(matches!(
        b.get_mut(&forged_owner),
        Err(Error::InvalidHandle)
    ));
    let wrong = Handle::<u64> {
        rep: old.rep,
        serial: old.serial,
        owner: old.owner,
        marker: PhantomData,
    };
    assert_eq!(a.remove(&wrong), Err(Error::InvalidHandle));
    assert_eq!(*a.get_mut(&old).unwrap(), 1);
    a.remove(&old).unwrap();
    let new = a.insert(3u32).unwrap();
    assert_eq!(old.rep, new.rep);
    assert!(matches!(a.get_mut(&old), Err(Error::InvalidHandle)));
    assert!(matches!(a.remove(&old), Err(Error::InvalidHandle)));
    assert_eq!(*a.get_mut(&new).unwrap(), 3);
}
#[test]
fn resource_owner_run_clear_releases_pending_future_and_all_pollables() {
    let (run, scope) = scope();
    let n = Arc::new(AtomicUsize::new(0));
    let probe = Probe(n.clone());
    let mut owner = Owner::new(&scope);
    let task = owner
        .insert(Task::new(
            &scope,
            Instant::now() + std::time::Duration::from_secs(1),
            async move {
                let _held = probe;
                std::future::pending().await
            },
        ))
        .unwrap();
    owner.subscribe(&task).unwrap();
    run.invalidate();
    assert!(matches!(
        owner.check(),
        Err(Error::Authority(Failure::Cancelled))
    ));
    assert!(owner.table.is_empty());
    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert!(matches!(
        owner.insert(5u32),
        Err(Error::Authority(Failure::Cancelled))
    ));
}
#[test]
fn resource_task_get_never_waits_and_cancel_drops_future_immediately() {
    let (_run, scope) = scope();
    let n = Arc::new(AtomicUsize::new(0));
    let probe = Probe(n.clone());
    let mut task = Task::new(
        &scope,
        Instant::now() + std::time::Duration::from_secs(1),
        async move {
            let _held = probe;
            std::future::pending().await
        },
    );
    assert!(task.get().is_none());
    assert_eq!(n.load(Ordering::SeqCst), 0);
    task.cancel();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert!(matches!(task.get(), Some(Err(task::Error::Cancelled))));
}
#[test]
fn resource_task_terminal_result_is_consumed_once_and_cancel_discards_ready() {
    let (_run, scope) = scope();
    for cancel in [false, true] {
        let mut task = Task::new(
            &scope,
            Instant::now() + std::time::Duration::from_secs(1),
            async {
                Err(task::Error::Http(
                    crate::http_authority::transport::Failure::Limit,
                ))
            },
        );
        runtime().block_on(task.ready());
        if cancel {
            task.cancel();
            assert!(matches!(task.get(), Some(Err(task::Error::Cancelled))));
        } else {
            assert!(matches!(task.get(), Some(Err(task::Error::Http(_)))));
        }
        assert!(matches!(task.get(), Some(Err(task::Error::Consumed))));
    }
}
#[test]
fn resource_task_readiness_observes_scope_and_absolute_deadline() {
    for expired in [false, true] {
        let (run, scope) = scope();
        let mut task = Task::new(
            &scope,
            Instant::now() + std::time::Duration::from_millis(50),
            std::future::pending(),
        );
        if !expired {
            run.invalidate();
        }
        runtime().block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(150), task.ready())
                .await
                .unwrap();
        });
        assert!(matches!(
            task.get(),
            Some(Err(task::Error::Authority(
                Failure::Cancelled | Failure::DeadlineExceeded
            )))
        ));
    }
}

#[test]
fn wasi_ready_probe_must_not_cancel_a_pending_task() {
    use wasmtime_wasi::p2::bindings::sync::io::poll::HostPollable;
    let (_run, scope) = scope();
    let mut owner = Owner::new(&scope);
    let task = owner
        .insert(Task::new(
            &scope,
            Instant::now() + std::time::Duration::from_secs(1),
            async {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                Err(task::Error::Consumed)
            },
        ))
        .unwrap();
    let pollable = owner.subscribe(&task).unwrap();
    for _ in 0..2 {
        assert!(
            !HostPollable::ready(&mut owner.table, Resource::new_borrow(pollable.rep)).unwrap()
        );
        assert!(owner.get_mut(&task).unwrap().get().is_none());
    }
    HostPollable::block(&mut owner.table, Resource::new_borrow(pollable.rep)).unwrap();
    assert!(matches!(
        owner.get_mut(&task).unwrap().get(),
        Some(Err(task::Error::Consumed))
    ));
}

#[test]
fn wasi_poll_winner_must_not_cancel_pending_losers() {
    use wasmtime_wasi::p2::bindings::sync::io::poll::Host;
    let (_run, scope) = scope();
    let mut owner = Owner::new(&scope);
    let mut handles = Vec::new();
    let mut pollables = Vec::new();
    for ms in [1, 80] {
        let task = owner
            .insert(Task::new(
                &scope,
                Instant::now() + std::time::Duration::from_secs(1),
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    Err(task::Error::Consumed)
                },
            ))
            .unwrap();
        pollables.push(owner.subscribe(&task).unwrap());
        handles.push(task);
    }
    assert_eq!(
        Host::poll(
            &mut owner.table,
            pollables
                .iter()
                .map(|h| Resource::new_borrow(h.rep))
                .collect()
        )
        .unwrap(),
        vec![0]
    );
    assert!(owner.get_mut(&handles[1]).unwrap().get().is_none());
    assert_eq!(
        Host::poll(
            &mut owner.table,
            vec![Resource::new_borrow(pollables[1].rep)]
        )
        .unwrap(),
        vec![0]
    );
    assert!(matches!(
        owner.get_mut(&handles[1]).unwrap().get(),
        Some(Err(task::Error::Consumed))
    ));
}
