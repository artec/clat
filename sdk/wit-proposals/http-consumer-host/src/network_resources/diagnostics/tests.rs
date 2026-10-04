use super::*;
use crate::network_resources::tests::scope;
use streams::HostOutputStream;
use wasmtime_wasi::p2::bindings::sync::io::poll::HostPollable;
#[test]
fn scheduler_diagnostics_share_one_finite_budget_even_after_reopening() {
    let (_run, scope) = scope();
    let mut owner = Owner::new(&scope);
    let stream = stderr::Host::get_stderr(&mut owner).unwrap();
    let child =
        HostOutputStream::subscribe(&mut owner, Resource::new_borrow(stream.rep())).unwrap();
    assert_eq!(owner.len(), 2);
    HostOutputStream::write(
        &mut owner,
        Resource::new_borrow(stream.rep()),
        vec![1; 32767],
    )
    .unwrap();
    assert!(matches!(
        HostOutputStream::write(&mut owner, Resource::new_borrow(stream.rep()), vec![2; 2]),
        Err(StreamError::Closed)
    ));
    assert!(matches!(
        HostOutputStream::write_zeroes(&mut owner, Resource::new_borrow(stream.rep()), u64::MAX),
        Err(StreamError::Closed)
    ));
    HostOutputStream::write(&mut owner, Resource::new_borrow(stream.rep()), vec![2]).unwrap();
    HostPollable::drop(&mut owner, child).unwrap();
    HostOutputStream::drop(&mut owner, stream).unwrap();
    assert!(owner.table.is_empty());
    let next = stderr::Host::get_stderr(&mut owner).unwrap();
    assert!(matches!(
        HostOutputStream::check_write(&mut owner, Resource::new_borrow(next.rep())),
        Err(StreamError::Closed)
    ));
    HostOutputStream::drop(&mut owner, next).unwrap();
    assert_eq!(owner.diagnostics.contents().len(), 32768);
    assert_eq!(owner.len(), 0);
}
#[test]
fn scheduler_error_conversion_never_allocates_a_hidden_resource() {
    let (_run, scope) = scope();
    let mut owner = Owner::new(&scope);
    let result = streams::Host::convert_stream_error(
        &mut owner,
        StreamError::LastOperationFailed(wasmtime::format_err!("private diagnostic")),
    )
    .unwrap();
    assert!(matches!(result, streams::StreamError::Closed));
    assert!(owner.table.is_empty());
    assert_eq!(owner.len(), 0);
}
#[test]
fn scheduler_diagnostics_revocation_and_subscriptions_use_same_owner_limit() {
    let (run, scope) = scope();
    let mut owner = Owner::new(&scope);
    let stream = stderr::Host::get_stderr(&mut owner).unwrap();
    for _ in 0..15 {
        HostOutputStream::subscribe(&mut owner, Resource::new_borrow(stream.rep())).unwrap();
    }
    assert!(HostOutputStream::subscribe(&mut owner, Resource::new_borrow(stream.rep())).is_err());
    run.invalidate();
    assert!(HostOutputStream::flush(&mut owner, Resource::new_borrow(stream.rep())).is_err());
    assert!(owner.table.is_empty());
    assert_eq!(owner.len(), 0);
    HostOutputStream::drop(&mut owner, stream).unwrap();
}
