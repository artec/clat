use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
struct Probe {
    name: String,
    calls: Arc<AtomicUsize>,
    metadata: ToolCapability,
    schema: Value,
}
impl Tool for Probe {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: "测试 test tool; API_KEY=supersecretvalue".into(),
            input_schema: self.schema.clone(),
            effect: ToolEffect::Write,
            strict: false,
        }
    }
    fn capability_metadata(&self) -> ToolCapability {
        self.metadata.clone()
    }
    fn invoke(&self, _: &Value, _: &Project, _: &CancelToken) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({}))
    }
}
fn register(registry: &ToolRegistry, name: &str, schema: Value) -> (ToolLease, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let lease = registry
        .register(
            PluginOwner::for_test(PluginId::new("test.capability")),
            Arc::new(Probe {
                name: name.into(),
                calls: calls.clone(),
                metadata: ToolCapability::native(
                    &["test", "测试"],
                    &["json"],
                    &["table"],
                    "local declaration, not aggregate verification",
                ),
                schema,
            }),
        )
        .unwrap();
    (lease, calls)
}
fn query(registry: &ToolRegistry, request: Value) -> Value {
    registry
        .query_capabilities(&request, &CancelToken::new())
        .unwrap()
}
#[test]
fn ga3_discovery_is_read_only_explainable_bounded_and_legacy_compatible() {
    let registry = ToolRegistry::new();
    let (_lease, calls) = register(&registry, "legacy", json!({"type":"object"}));
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    for text in ["TEST", "测试"] {
        let page = query(
            &registry,
            json!({"text":text,"input":"json","output":"table","scope":"local"}),
        );
        let row = &page["candidates"][0];
        assert_eq!(row["owner"], "test.capability");
        assert_eq!(row["effect"], "writes files");
        assert!(row["missing"].as_array().unwrap().len() >= 2);
        assert!(!page.to_string().contains("supersecretvalue"));
        assert_eq!(page["confirmed_path"], false);
        let detail = query(&registry, json!({"id":row["id"]}));
        assert_eq!(
            serde_json::from_str::<Value>(detail["schema"]["text"].as_str().unwrap()).unwrap(),
            json!({"type":"object"})
        );
        assert_eq!(detail["schema"]["complete"], true);
        assert!(
            detail["authorization"]
                .as_str()
                .unwrap()
                .contains("invocation")
        );
    }
    assert_eq!(
        query(&registry, json!({"output":"pdf"}))["candidates"],
        json!([])
    );
    assert_eq!(
        query(&registry, json!({"source":"some-other-owner"}))["candidates"],
        json!([])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn ga3_pages_are_stable_json_bounded_and_expire_with_lease_or_context() {
    let registry = ToolRegistry::new();
    let leases: Vec<_> = (0..80)
        .map(|i| register(&registry, &format!("tool-{i:03}"), json!({"type":"object"})).0)
        .collect();
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    let mut request = json!({"limit":4});
    let mut names = Vec::new();
    loop {
        let page = query(&registry, request.clone());
        assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
        for row in page["candidates"].as_array().unwrap() {
            names.push(row["name"].as_str().unwrap().to_owned());
        }
        if page["next_cursor"].is_null() {
            break;
        }
        request["cursor"] = page["next_cursor"].clone();
        assert!(names.len() <= 80);
    }
    assert_eq!(names.len(), 80);
    assert!(names.windows(2).all(|p| p[0] < p[1]));
    let old = query(&registry, json!({"limit":1}));
    let detail = json!({"id":old["candidates"][0]["id"]});
    for lease in leases {
        lease.revoke().unwrap();
    }
    assert!(
        registry
            .query_capabilities(&detail, &CancelToken::new())
            .is_err()
    );
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    assert!(
        registry
            .query_capabilities(
                &json!({"cursor":old["next_cursor"],"limit":1}),
                &CancelToken::new()
            )
            .is_err()
    );
    registry.clear_capabilities();
    assert!(
        registry
            .query_capabilities(&json!({}), &CancelToken::new())
            .is_err()
    );
}
#[test]
fn ga3_frozen_visibility_never_falls_back_to_registry_or_name_guess() {
    let registry = ToolRegistry::new();
    let (_write, calls) = register(&registry, "secret-write", json!({"hidden":"do not reveal"}));
    let plan = ToolAccessPolicy::plan_mode();
    registry.install_capabilities(registry.capability_view(Some(&plan)));
    for request in [
        json!({}),
        json!({"name":"secret-write"}),
        json!({"text":"secret"}),
        json!({"output":"table"}),
    ] {
        let page = query(&registry, request);
        assert_eq!(page["candidates"], json!([]));
        assert!(!page.to_string().contains("secret-write"));
    }
    assert!(
        registry
            .query_capabilities(&json!({"id":"secret-write"}), &CancelToken::new())
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn ga3_schema_chunks_respect_escape_bytes_unicode_and_explicit_omission() {
    let registry = ToolRegistry::new();
    let schema = json!({"type":"object","description":"\u{0001}\\\"中文".repeat(1500)});
    let (_lease, _) = register(&registry, "escaped", schema.clone());
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    let page = query(&registry, json!({}));
    let id = page["candidates"][0]["id"].clone();
    let mut offset = 0;
    let mut actual = String::new();
    let mut pages = 0;
    loop {
        let detail = query(&registry, json!({"id":id,"schema_offset":offset}));
        assert!(serde_json::to_vec(&detail).unwrap().len() <= 4096);
        assert!(serde_json::to_vec(&detail["schema"]["text"]).unwrap().len() <= 1024);
        actual.push_str(detail["schema"]["text"].as_str().unwrap());
        pages += 1;
        if detail["schema"]["next_offset"].is_null() {
            break;
        }
        let next = detail["schema"]["next_offset"].as_u64().unwrap() as usize;
        assert!(next > offset);
        offset = next;
        assert!(pages < 1000);
    }
    assert_eq!(serde_json::from_str::<Value>(&actual).unwrap(), schema);
    assert!(pages > 1);
    let (_huge, _) = register(&registry, "huge", json!({"description":"x".repeat(70000)}));
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    let p = query(&registry, json!({"name":"huge"}));
    let d = query(&registry, json!({"id":p["candidates"][0]["id"]}));
    assert_eq!(d["schema"]["available"], false);
    assert_eq!(d["schema"]["complete"], false);
}
#[test]
fn ga3_cancel_limits_lifetime_and_concurrent_close_are_fail_closed() {
    let registry = Arc::new(ToolRegistry::new());
    let life = Arc::new(());
    let tool = Probe {
        name: "external".into(),
        calls: Arc::new(AtomicUsize::new(0)),
        schema: json!({}),
        metadata: ToolCapability {
            source: "mcp",
            adapter: "local-fixture".into(),
            lifetime: Some(Arc::downgrade(&life)),
            ..Default::default()
        },
    };
    let _lease = registry
        .register(
            PluginOwner::for_test(PluginId::new("adapter")),
            Arc::new(tool),
        )
        .unwrap();
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(registry.query_capabilities(&json!({}), &cancel).is_err());
    for args in [
        json!({"text":"x".repeat(257)}),
        json!({"limit":0}),
        json!({"limit":5}),
        json!({"unknown":true}),
    ] {
        assert!(
            registry
                .query_capabilities(&args, &CancelToken::new())
                .is_err()
        );
    }
    let page = query(&registry, json!({}));
    assert!(
        page["candidates"][0]["readiness"]
            .as_str()
            .unwrap()
            .contains("unknown")
    );
    drop(life);
    assert!(
        query(&registry, json!({}))["candidates"][0]["readiness"]
            .as_str()
            .unwrap()
            .contains("blocked")
    );
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let r = registry.clone();
            scope.spawn(move || {
                for _ in 0..50 {
                    let _ = r.query_capabilities(&json!({}), &CancelToken::new());
                }
            });
        }
        registry.clear_capabilities();
    });
    assert!(
        registry
            .query_capabilities(&json!({}), &CancelToken::new())
            .is_err()
    );
}

#[test]
fn ga3_all_policy_views_share_exact_model_definitions_for_name_pages_and_alternatives() {
    let registry = ToolRegistry::new();
    let leases: Vec<_> = [
        "view_image",
        "delegate_readonly",
        "read_task",
        "update_task",
        "verify_task_acceptance",
        "propose_task_completion",
        "write_file",
    ]
    .into_iter()
    .map(|name| register(&registry, name, json!({"private":name})).0)
    .collect();
    for policy in [
        ToolAccessPolicy::all(),
        ToolAccessPolicy::plan_mode(),
        ToolAccessPolicy::readonly_child(),
        ToolAccessPolicy::all()
            .with_task_progress(true)
            .with_subagents(true)
            .with_view_image(true),
    ] {
        let expected = registry.definitions_for(&policy);
        let view = registry.capability_view(Some(&policy));
        assert_eq!(view.definitions(), expected);
        registry.install_capabilities(view);
        for name in [
            "view_image",
            "delegate_readonly",
            "read_task",
            "update_task",
            "verify_task_acceptance",
            "propose_task_completion",
            "write_file",
        ] {
            let visible = expected.iter().any(|d| d.name == name);
            let p = query(&registry, json!({"name":name}));
            assert_eq!(!p["candidates"].as_array().unwrap().is_empty(), visible);
            let candidates = query(&registry, json!({"name":name,"output":"table"}));
            assert_eq!(
                !candidates["candidates"].as_array().unwrap().is_empty(),
                visible
            );
        }
    }
    assert!(!leases.is_empty());
}

#[test]
fn ga3_unannotated_tool_stays_callable_and_unknown_and_source_constraints_do_not_merge() {
    struct Legacy;
    impl Tool for Legacy {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "legacy".into(),
                description: "old implementation".into(),
                input_schema: json!({}),
                effect: ToolEffect::Read,
                strict: false,
            }
        }
        fn invoke(&self, _: &Value, _: &Project, _: &CancelToken) -> Result<Value, ToolError> {
            Ok(json!({"legacy":true}))
        }
    }
    let registry = ToolRegistry::new();
    let lease = registry
        .register(
            PluginOwner::for_test(PluginId::new("old-owner")),
            Arc::new(Legacy),
        )
        .unwrap();
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    let p = query(
        &registry,
        json!({"source":"old-owner","output":"pdf","scope":"local"}),
    );
    assert_eq!(p["candidates"][0]["source"], "undeclared");
    assert!(
        p["candidates"][0]["matches"]
            .to_string()
            .contains("undeclared")
    );
    assert_eq!(p["confirmed_path"], false);
    assert_eq!(
        query(&registry, json!({"source":"different-owner"}))["candidates"],
        json!([])
    );
    assert_eq!(
        registry
            .get("legacy")
            .unwrap()
            .invoke(
                &json!({}),
                &Project::new(std::env::temp_dir()),
                &CancelToken::new()
            )
            .unwrap(),
        json!({"legacy":true})
    );
    lease.revoke().unwrap();
}

#[test]
fn ga3_hostile_metadata_cannot_squeeze_identity_or_missing_conditions_out_of_json() {
    let registry = ToolRegistry::new();
    let nasty = "\u{0001}\"\\\n".repeat(1000);
    struct Hostile(Probe, String);
    impl Tool for Hostile {
        fn definition(&self) -> ToolDefinition {
            let mut d = self.0.definition();
            d.description = self.1.clone();
            d
        }
        fn capability_metadata(&self) -> ToolCapability {
            self.0.metadata.clone()
        }
        fn invoke(&self, _: &Value, _: &Project, _: &CancelToken) -> Result<Value, ToolError> {
            panic!("discovery executed hostile tool")
        }
    }
    let _lease = registry
        .register(
            PluginOwner::for_test(PluginId::new("hostile")),
            Arc::new(Hostile(
                Probe {
                    name: "malicious".into(),
                    calls: Arc::new(AtomicUsize::new(0)),
                    schema: json!({"type":"object"}),
                    metadata: ToolCapability {
                        source: "mcp",
                        adapter: nasty.clone(),
                        tags: vec![nasty.clone(); 8],
                        inputs: vec![nasty.clone(); 8],
                        outputs: vec![nasty.clone(); 8],
                        limits: nasty.clone(),
                        ..Default::default()
                    },
                },
                nasty,
            )),
        )
        .unwrap();
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    let page = query(&registry, json!({}));
    assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
    let row = &page["candidates"][0];
    assert_eq!(row["name"], "malicious");
    assert!(!row["id"].is_null());
    assert_eq!(row["description_complete"], false);
    assert_eq!(row["missing"].as_array().unwrap().len(), 2);
    assert!(row["readiness"].as_str().unwrap().contains("unknown"));
    let detail = query(&registry, json!({"id":row["id"]}));
    assert_eq!(detail["schema"]["complete"], true);
    assert!(serde_json::to_vec(&detail).unwrap().len() <= 4096);
}

#[test]
fn ga3_reload_never_reuses_registration_identity_or_falls_back_to_old_source() {
    let registry = ToolRegistry::new();
    let (lease, _) = register(&registry, "same-name", json!({"type":"object"}));
    registry.freeze().unwrap();
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    let old = query(&registry, json!({"name":"same-name"}));
    let old_row = &old["candidates"][0];
    lease.revoke().unwrap();
    registry.reopen_for_plugin_reload().unwrap();
    let tool = Probe {
        name: "same-name".into(),
        calls: Arc::new(AtomicUsize::new(0)),
        metadata: ToolCapability::external("wasm", "replacement", "external"),
        schema: json!({"required":["new-contract"]}),
    };
    let _lease = registry
        .register(
            PluginOwner::for_test(PluginId::new("replacement-owner")),
            Arc::new(tool),
        )
        .unwrap();
    registry.freeze().unwrap();
    assert!(
        registry
            .query_capabilities(&json!({"id":old_row["id"]}), &CancelToken::new())
            .is_err()
    );
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    let new = query(
        &registry,
        json!({"name":"same-name","source":"replacement-owner"}),
    );
    assert_ne!(
        new["candidates"][0]["registration"],
        old_row["registration"]
    );
    assert_eq!(new["candidates"][0]["source"], "wasm");
    assert_eq!(
        query(
            &registry,
            json!({"name":"same-name","source":"test.capability"})
        )["candidates"],
        json!([])
    );
    assert!(
        registry
            .query_capabilities(&json!({"id":old_row["id"]}), &CancelToken::new())
            .is_err()
    );
}

fn reassemble(registry: &ToolRegistry, view: Option<&CapabilityView>, id: &Value) -> Value {
    let mut offset = 0;
    let mut text = String::new();
    loop {
        let request = json!({"id":id,"schema_offset":offset});
        let detail = match view {
            Some(view) => registry
                .query_capability_view(view, &request, &CancelToken::new())
                .unwrap(),
            None => query(registry, request),
        };
        assert!(!detail.to_string().contains("supersecretvalue"));
        assert_eq!(
            detail["schema"]["complete"],
            offset == 0 && detail["schema"]["next_offset"].is_null()
        );
        assert!(serde_json::to_vec(&detail).unwrap().len() <= 4096);
        text.push_str(detail["schema"]["text"].as_str().unwrap());
        if detail["schema"]["next_offset"].is_null() {
            break;
        }
        let next = detail["schema"]["next_offset"].as_u64().unwrap();
        assert!(next > offset);
        offset = next;
        assert!(offset < 65536);
    }
    serde_json::from_str(&text).unwrap()
}

#[test]
fn ga3_rework_shared_schema_annotations_never_publish_secret_assignment() {
    let registry = ToolRegistry::new();
    let annotation = format!(
        "{}API_KEY=supersecretvalue{}",
        "中文\\\"".repeat(250),
        "z".repeat(2500)
    );
    let schema = json!({"type":"object", "description":annotation,
        "properties":{"api_key":{"type":"string","description":annotation}},
        "$defs":{"part":{"$comment":annotation}},
        "allOf":[{"title":annotation}], "items":{"description":annotation},
        "const":{"description":"ordinary data", "type":"literal"},
        "enum":[{"description":"unchanged data"}],
        "default":{"description":"default data"},
        "examples":[{"description":"example data"}],
        "x-extension":{"description":"extension data"},
        "required":["api_key"], "additionalProperties":false});
    let (_lease, calls) = register(&registry, "external-schema", schema.clone());
    let model = registry.definitions().pop().unwrap().input_schema;
    assert!(!model.to_string().contains("supersecretvalue"));
    for key in [
        "type",
        "required",
        "const",
        "enum",
        "default",
        "examples",
        "x-extension",
        "additionalProperties",
    ] {
        assert_eq!(model[key], schema[key], "validation data changed: {key}");
    }
    let idle = registry.user_capability_view("test-session");
    let idle_page = registry
        .query_capability_view(
            &idle,
            &json!({"name":"external-schema"}),
            &CancelToken::new(),
        )
        .unwrap();
    registry.install_capabilities(registry.capability_view(Some(&ToolAccessPolicy::all())));
    let page = query(&registry, json!({"name":"external-schema"}));
    assert_eq!(
        reassemble(&registry, Some(&idle), &idle_page["candidates"][0]["id"]),
        model
    );
    assert_eq!(
        reassemble(&registry, None, &page["candidates"][0]["id"]),
        model
    );
    assert!(
        model["description"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn ga3_rework_secret_constraints_reject_registration_without_rewriting_or_partial_entry() {
    let registry = ToolRegistry::new();
    let (_healthy, _) = register(&registry, "healthy", json!({"type":"object"}));
    let secret = "API_KEY=supersecretvalue";
    let schemas = [
        json!({"properties":{secret:{"type":"string"}}}),
        json!({"required":[secret]}),
        json!({"enum":[secret]}),
        json!({"const":secret}),
        json!({"pattern":secret}),
        json!({"$ref":secret}),
        json!({"const":{"description":secret}}),
        json!({"default":{"api_key":"supersecretvalue"}}),
        json!({"examples":[{"description":secret}]}),
        json!({"x-extension":{"description":secret}}),
        json!({"api_key":"supersecretvalue"}),
    ];
    let revision = registry.inner.read().unwrap().revision;
    for schema in schemas {
        let result = registry.register(
            PluginOwner::for_test(PluginId::new("unsafe")),
            Arc::new(Probe {
                name: "unsafe".into(),
                calls: Arc::new(AtomicUsize::new(0)),
                metadata: ToolCapability::default(),
                schema,
            }),
        );
        assert!(matches!(result, Err(ToolRegistryError::UnsafeSchema)));
        let error = result.err().unwrap().to_string();
        assert!(!error.contains("supersecretvalue"));
        assert!(registry.get("unsafe").is_none());
        assert_eq!(registry.inner.read().unwrap().revision, revision);
    }
    assert_eq!(registry.definitions().len(), 1);
}
