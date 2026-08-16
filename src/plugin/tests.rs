use super::effect::EffectScope;
use super::manager::{CatalogError, PluginManagerError, ScopeCloseError};
use super::service::ServiceError;
use super::*;
use std::sync::{Arc, Mutex};

trait NumberService: Send + Sync {
    fn value(&self) -> i32;
}

struct Number(i32);

impl NumberService for Number {
    fn value(&self) -> i32 {
        self.0
    }
}

const NUMBER_ID: ServiceId = ServiceId::new("test.number");
const OTHER_ID: ServiceId = ServiceId::new("test.other");
const THIRD_ID: ServiceId = ServiceId::new("test.third");
const EMPTY_ID: ServiceId = ServiceId::new("");
const EMPTY_IDS: &[ServiceId] = &[EMPTY_ID];
const NUMBER_KEY: ServiceKey<dyn NumberService> = ServiceKey::new(NUMBER_ID);
const STRING_KEY: ServiceKey<String> = ServiceKey::new(NUMBER_ID);
const OTHER_KEY: ServiceKey<String> = ServiceKey::new(OTHER_ID);
const THIRD_KEY: ServiceKey<String> = ServiceKey::new(THIRD_ID);

type MountFn = dyn Fn(&mut PluginContext<'_>) -> Result<(), PluginError> + Send + Sync;

struct TestPlugin {
    descriptor: &'static PluginDescriptor,
    mount: Arc<MountFn>,
}

impl Plugin for TestPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        self.descriptor
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        (self.mount)(context)
    }
}

fn plugin(
    descriptor: &'static PluginDescriptor,
    mount: impl Fn(&mut PluginContext<'_>) -> Result<(), PluginError> + Send + Sync + 'static,
) -> Arc<dyn Plugin> {
    Arc::new(TestPlugin {
        descriptor,
        mount: Arc::new(mount),
    })
}

fn descriptor(
    id: &'static str,
    scope: ScopeKind,
    provides: &'static [ServiceId],
    requires: &'static [ServiceId],
    optional: &'static [ServiceId],
) -> &'static PluginDescriptor {
    Box::leak(Box::new(PluginDescriptor {
        id: PluginId::new(id),
        scope,
        provides,
        requires,
        optional,
    }))
}

#[test]
fn typed_services_resolve_and_mismatched_keys_fail() {
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    let provider = descriptor("provider", ScopeKind::Bootstrap, &[NUMBER_ID], &[], &[]);
    manager
        .mount_all(vec![plugin(provider, |context| {
            context
                .provide(NUMBER_KEY, Arc::new(Number(7)))
                .map_err(|error| PluginError::new(error.to_string()))
        })])
        .expect("mount");
    assert_eq!(manager.require(NUMBER_KEY).expect("service").value(), 7);
    assert!(matches!(
        manager.require(STRING_KEY),
        Err(ServiceError::TypeMismatch(NUMBER_ID))
    ));
}

#[test]
fn plugins_cannot_consume_services_their_descriptor_did_not_declare() {
    let provider = descriptor("provider", ScopeKind::Bootstrap, &[NUMBER_ID], &[], &[]);
    let undeclared = descriptor("undeclared", ScopeKind::Bootstrap, &[], &[], &[]);
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    let error = manager
        .mount_all(vec![
            plugin(provider, |context| {
                context
                    .provide(NUMBER_KEY, Arc::new(Number(7)))
                    .map_err(|error| PluginError::new(error.to_string()))
            }),
            plugin(undeclared, |context| {
                context
                    .require(NUMBER_KEY)
                    .map(|_| ())
                    .map_err(|error| PluginError::new(error.to_string()))
            }),
        ])
        .expect_err("undeclared dependency must fail during its transaction");
    let PluginManagerError::Start(error) = error else {
        panic!("unexpected error");
    };
    assert!(error.primary.to_string().contains("undeclared dependency"));
    assert!(
        manager.require(NUMBER_KEY).is_err(),
        "rollback removes provider"
    );
}

#[test]
fn catalog_validation_happens_before_the_first_mount() {
    let mounted = Arc::new(Mutex::new(Vec::new()));
    let first = descriptor("duplicate", ScopeKind::Bootstrap, &[NUMBER_ID], &[], &[]);
    let second = descriptor("duplicate", ScopeKind::Bootstrap, &[OTHER_ID], &[], &[]);
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    let error = manager
        .mount_all(vec![
            plugin(first, {
                let mounted = Arc::clone(&mounted);
                move |_| {
                    mounted.lock().unwrap().push("first");
                    Ok(())
                }
            }),
            plugin(second, |_| Ok(())),
        ])
        .expect_err("duplicate ids fail");
    assert!(matches!(
        error,
        PluginManagerError::Catalog(CatalogError::DuplicatePlugin(_))
    ));
    assert!(mounted.lock().unwrap().is_empty());

    let empty_service = descriptor("empty-service", ScopeKind::Bootstrap, EMPTY_IDS, &[], &[]);
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    assert!(matches!(
        manager.mount_all(vec![plugin(empty_service, |_| Ok(()))]),
        Err(PluginManagerError::Catalog(
            CatalogError::EmptyServiceId { .. }
        ))
    ));
}

#[test]
fn rejects_scope_mismatch_missing_dependencies_and_duplicate_services() {
    let wrong_scope = descriptor("wrong", ScopeKind::Run, &[], &[], &[]);
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    assert!(matches!(
        manager.mount_all(vec![plugin(wrong_scope, |_| Ok(()))]),
        Err(PluginManagerError::Catalog(
            CatalogError::ScopeMismatch { .. }
        ))
    ));

    let missing = descriptor("missing", ScopeKind::Bootstrap, &[], &[NUMBER_ID], &[]);
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    assert!(matches!(
        manager.mount_all(vec![plugin(missing, |_| Ok(()))]),
        Err(PluginManagerError::Catalog(
            CatalogError::MissingDependency { .. }
        ))
    ));

    let first = descriptor("first", ScopeKind::Bootstrap, &[NUMBER_ID], &[], &[]);
    let second = descriptor("second", ScopeKind::Bootstrap, &[NUMBER_ID], &[], &[]);
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    assert!(matches!(
        manager.mount_all(vec![plugin(first, |_| Ok(())), plugin(second, |_| Ok(()))]),
        Err(PluginManagerError::Catalog(
            CatalogError::DuplicateService { .. }
        ))
    ));
}

#[test]
fn optional_dependencies_order_when_present_and_do_nothing_when_absent() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let dependent = descriptor(
        "dependent",
        ScopeKind::Bootstrap,
        &[OTHER_ID],
        &[],
        &[NUMBER_ID],
    );
    let provider = descriptor("provider", ScopeKind::Bootstrap, &[NUMBER_ID], &[], &[]);
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    manager
        .mount_all(vec![
            plugin(dependent, {
                let log = Arc::clone(&log);
                move |context| {
                    log.lock().unwrap().push("dependent");
                    context
                        .provide(OTHER_KEY, Arc::new("other".into()))
                        .map_err(|error| PluginError::new(error.to_string()))
                }
            }),
            plugin(provider, {
                let log = Arc::clone(&log);
                move |context| {
                    log.lock().unwrap().push("provider");
                    context
                        .provide(NUMBER_KEY, Arc::new(Number(1)))
                        .map_err(|error| PluginError::new(error.to_string()))
                }
            }),
        ])
        .expect("mount");
    assert_eq!(*log.lock().unwrap(), ["provider", "dependent"]);

    let absent = descriptor("absent-ok", ScopeKind::Bootstrap, &[], &[], &[THIRD_ID]);
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    manager
        .mount_all(vec![plugin(absent, |_| Ok(()))])
        .expect("missing optional dependency is ignored");
}

#[test]
fn required_and_optional_cycles_fail() {
    let first = descriptor(
        "first-cycle",
        ScopeKind::Bootstrap,
        &[NUMBER_ID],
        &[OTHER_ID],
        &[],
    );
    let second = descriptor(
        "second-cycle",
        ScopeKind::Bootstrap,
        &[OTHER_ID],
        &[],
        &[NUMBER_ID],
    );
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    assert!(matches!(
        manager.mount_all(vec![plugin(first, |_| Ok(())), plugin(second, |_| Ok(()))]),
        Err(PluginManagerError::Catalog(CatalogError::DependencyCycle(
            _
        )))
    ));
}

#[test]
fn stable_catalog_order_and_reverse_teardown_are_deterministic() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let make = |name: &'static str, id: ServiceId, key: ServiceKey<String>| {
        let provides = Box::leak(vec![id].into_boxed_slice());
        let descriptor = descriptor(name, ScopeKind::Bootstrap, provides, &[], &[]);
        plugin(descriptor, {
            let log = Arc::clone(&log);
            move |context| {
                log.lock().unwrap().push(format!("mount:{name}"));
                let dispose_log = Arc::clone(&log);
                context.defer(move || {
                    dispose_log.lock().unwrap().push(format!("close:{name}"));
                    Ok(())
                });
                context
                    .provide(key, Arc::new(name.into()))
                    .map_err(|error| PluginError::new(error.to_string()))
            }
        })
    };
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    manager
        .mount_all(vec![
            make("first", NUMBER_ID, STRING_KEY),
            make("second", OTHER_ID, OTHER_KEY),
            make("third", THIRD_ID, THIRD_KEY),
        ])
        .expect("mount");
    manager.close().expect("close");
    assert_eq!(
        *log.lock().unwrap(),
        [
            "mount:first",
            "mount:second",
            "mount:third",
            "close:third",
            "close:second",
            "close:first"
        ]
    );
    manager.close().expect("second close is idempotent");
}

#[test]
fn mount_failure_rolls_back_current_then_previous_and_keeps_all_errors() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let first = descriptor("first", ScopeKind::Bootstrap, &[NUMBER_ID], &[], &[]);
    let failing = descriptor(
        "failing",
        ScopeKind::Bootstrap,
        &[OTHER_ID],
        &[NUMBER_ID],
        &[],
    );
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    let error = manager
        .mount_all(vec![
            plugin(first, {
                let log = Arc::clone(&log);
                move |context| {
                    let dispose_log = Arc::clone(&log);
                    context.defer(move || {
                        dispose_log.lock().unwrap().push("close:first");
                        Err(DisposeError::new("first cleanup failed"))
                    });
                    context
                        .provide(NUMBER_KEY, Arc::new(Number(1)))
                        .map_err(|error| PluginError::new(error.to_string()))
                }
            }),
            plugin(failing, {
                let log = Arc::clone(&log);
                move |context| {
                    let dispose_log = Arc::clone(&log);
                    context.defer(move || {
                        dispose_log.lock().unwrap().push("close:failing");
                        Err(DisposeError::new("failing cleanup failed"))
                    });
                    Err(PluginError::new("mount failed"))
                }
            }),
        ])
        .expect_err("mount fails");
    let PluginManagerError::Start(error) = error else {
        panic!("unexpected error");
    };
    assert_eq!(error.primary.to_string(), "mount failed");
    assert_eq!(error.rollback_failures.len(), 2);
    assert_eq!(*log.lock().unwrap(), ["close:failing", "close:first"]);
    assert!(matches!(
        manager.require(NUMBER_KEY),
        Err(ServiceError::Missing(NUMBER_ID))
    ));
}

#[test]
fn disposer_panics_do_not_block_later_cleanup_and_are_not_retried() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut effects = EffectScope::new();
    let first_log = Arc::clone(&log);
    effects.defer(move || {
        first_log.lock().unwrap().push("first");
        Ok(())
    });
    effects.defer(|| panic!("boom"));
    let error = effects.close().expect_err("panic is aggregated");
    assert_eq!(error.into_errors().len(), 1);
    assert_eq!(*log.lock().unwrap(), ["first"]);
    effects.close().expect("failed disposer is not retried");
}

#[test]
fn child_scopes_inherit_services_cannot_override_and_block_parent_close() {
    let provider = descriptor(
        "bootstrap-provider",
        ScopeKind::Bootstrap,
        &[NUMBER_ID],
        &[],
        &[],
    );
    let mut parent = PluginManager::root(ScopeKind::Bootstrap);
    parent
        .mount_all(vec![plugin(provider, |context| {
            context
                .provide(NUMBER_KEY, Arc::new(Number(9)))
                .map_err(|error| PluginError::new(error.to_string()))
        })])
        .expect("parent mount");
    let mut child = parent.child(ScopeKind::TrustedProject).expect("child");
    assert_eq!(child.require(NUMBER_KEY).unwrap().value(), 9);
    assert!(matches!(
        parent.close(),
        Err(ScopeCloseError::ActiveChildren(1))
    ));

    let overriding = descriptor(
        "override",
        ScopeKind::TrustedProject,
        &[NUMBER_ID],
        &[],
        &[],
    );
    assert!(matches!(
        child.mount_all(vec![plugin(overriding, |_| Ok(()))]),
        Err(PluginManagerError::Catalog(
            CatalogError::ParentServiceOverride { .. }
        ))
    ));
    child.close().expect("child close");
    parent.close().expect("parent closes after child");
}

#[test]
fn actual_provides_must_match_the_descriptor() {
    let lies = descriptor("lies", ScopeKind::Bootstrap, &[NUMBER_ID], &[], &[]);
    let mut manager = PluginManager::root(ScopeKind::Bootstrap);
    let error = manager
        .mount_all(vec![plugin(lies, |_| Ok(()))])
        .expect_err("missing actual service fails the plugin transaction");
    assert!(matches!(error, PluginManagerError::Start(_)));
}
