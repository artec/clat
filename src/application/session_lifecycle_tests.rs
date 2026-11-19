//! SL-1 会话生命周期不变量（2026-11-19 立项）测试：归档=停摆
//! （INV-SL2）、活跃域零删除 + 删除显式两段式 + 无孤儿（INV-SL1/4）、
//! 共享引用守卫（INV-SL5）。全部从不变量推导，先红于实现。
use super::*;

fn start_run_text(
    application: &mut TrustedProjectApplication,
    prompt: &str,
) -> Result<crate::RunHandle, ApplicationError> {
    let (completion, _receiver) = mpsc::channel();
    application.start_run(ApplicationRunRequest {
        message: crate::message::PendingMessage::text(prompt),
        asker: None,
        approver: allow_all_approver(),
        events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
        completion,
    })
}

/// INV-SL2：归档会话不接纳新 run。提交明确拒绝（`session.archived`），
/// 零 journal 写入（committed 序号不动）、零目录物化（会话数不变）；
/// 浏览（switch 到归档会话）不受影响，提交在浏览态同样拒绝；恢复后
/// 立即可用。pre-fix：静默发进已归档会话（schedules/ACP 已过滤、
/// prompt 例外）。
#[test]
fn archived_sessions_reject_run_admission_without_writes_or_materialization() {
    let (storage, root) = roots("sl1-archived-admission");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    run(&mut app, "first turn in A").unwrap();
    let a = app.current_session_id().unwrap();
    app.new_session().unwrap();
    run(&mut app, "second turn in B").unwrap();
    let b = app.current_session_id().unwrap();
    let committed = app.committed_seq().unwrap();
    app.set_session_organization(b.as_str(), None, Some(true), true)
        .unwrap();
    let error = match start_run_text(&mut app, "into archived") {
        Err(error) => error,
        Ok(_) => panic!("run admission into an archived session must be rejected"),
    };
    assert_eq!(error.presentation().key, "session.archived");
    assert_eq!(app.committed_seq().unwrap(), committed);
    assert_eq!(app.list_sessions().unwrap().len(), 2);
    // 活跃域不受影响；归档列表浏览态（switch 到归档会话）仍可，
    // 但提交在任何选择路径下都被拒绝。
    app.switch_session(a).unwrap();
    run(&mut app, "active domain unaffected").unwrap();
    app.switch_session(b.clone()).unwrap();
    let error = match start_run_text(&mut app, "browse then submit") {
        Err(error) => error,
        Ok(_) => panic!("browsing an archived session must not re-enable submission"),
    };
    assert_eq!(error.presentation().key, "session.archived");
    app.set_session_organization(b.as_str(), None, Some(false), false)
        .unwrap();
    run(&mut app, "restored").unwrap();
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

/// INV-SL1/4：删除只存在于归档域。未归档 → `session.delete_not_archived`；
/// 未确认 → 显式拒绝；不存在的 id → 明确 "does not exist"（幂等重删同款，
/// 不 panic）。删除成功后目录与列表无孤儿，历史搜索（懒同步清理后）
/// 无鬼影；浏览态（打开中）直接删除成功且选择腾空到 Fresh（见尾腿）。
#[test]
fn delete_lives_only_in_the_archived_domain_and_leaves_no_orphans() {
    let (storage, root) = roots("sl1-delete-domain");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    run(&mut app, "victim session content").unwrap();
    let victim = app.current_session_id().unwrap();
    assert_eq!(
        app.delete_session(victim.as_str(), true)
            .unwrap_err()
            .presentation()
            .key,
        "session.delete_not_archived"
    );
    app.set_session_organization(victim.as_str(), None, Some(true), true)
        .unwrap();
    // 归档当前会话后选择移开（SL-U1/U2 语义下的常态）。
    app.new_session().unwrap();
    run(&mut app, "keeper session").unwrap();
    let keeper = app.current_session_id().unwrap();
    assert!(
        app.delete_session(victim.as_str(), false)
            .unwrap_err()
            .to_string()
            .contains("confirmation required")
    );
    assert!(
        app.delete_session("00000000-0000-0000-0000-000000000000", true)
            .unwrap_err()
            .to_string()
            .contains("does not exist")
    );
    app.delete_session(victim.as_str(), true).unwrap();
    assert!(
        app.list_sessions()
            .unwrap()
            .iter()
            .all(|s| s.id.as_str() != victim.as_str())
    );
    // 被删会话不在任何查询：历史搜索经懒同步清理后无命中。
    let hits = app
        .history_reader()
        .search(
            HistorySearch {
                query: "victim session content".into(),
                ..Default::default()
            },
            &crate::CancelToken::new(),
        )
        .unwrap()
        .hits;
    assert!(
        hits.iter()
            .all(|hit| hit.reference.session != victim.as_str())
    );
    // 目录与记录无孤儿：会话目录已整删；控制存储遗忘（flags +
    // sessionIds）；纯缓存（session_projcache）已随上面的列表刷新，
    // 搜索索引随上面的查询清理——此刻全树不再提及被删 id。
    assert!(
        !crate::test_support::storage_tree_mentions(&storage, victim.as_str()),
        "victim session directory must be fully removed"
    );
    // 幂等重删：明确拒绝，不复活。
    assert!(
        app.delete_session(victim.as_str(), true)
            .unwrap_err()
            .to_string()
            .contains("does not exist")
    );
    // 浏览态直接删除（2026-11-19 负责人驳回"打开中拒绝"设计）：切到
    // 归档的 keeper 浏览，从删除入口直接删——成功且选择腾空到 Fresh
    //（与 /new 同语义，writer 先 quiesce），目录/记录无残留，后续提交
    // 物化全新会话。
    app.set_session_organization(keeper.as_str(), None, Some(true), true)
        .unwrap();
    app.switch_session(keeper.clone()).unwrap();
    assert!(
        app.delete_session(keeper.as_str(), true).unwrap(),
        "deleting the open archived conversation must succeed and report the jump"
    );
    assert!(app.current_session_id().is_none());
    assert!(
        app.list_sessions()
            .unwrap()
            .iter()
            .all(|s| s.id.as_str() != keeper.as_str())
    );
    assert!(!crate::test_support::storage_tree_mentions(
        &storage,
        keeper.as_str()
    ));
    run(&mut app, "fresh conversation after delete").unwrap();
    assert_ne!(app.current_session_id().as_ref(), Some(&keeper));
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

/// INV-SL5：被其他会话冻结 session-share 引用的会话不可删除——
/// `session.delete_share_referenced` 点名引用方标题；引用方自身删除后
/// 引用解除，目标可删（默认拒绝，不做自动解除）。
#[test]
fn delete_refuses_share_referenced_sessions_until_references_are_gone() {
    let (storage, root) = roots("sl1-delete-share-guard");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    if let Some(worker) = &mut app.title_worker {
        worker.shutdown().unwrap();
    }
    run(&mut app, "shared source content").unwrap();
    let source = app.current_session_id().unwrap();
    let mut reference = app
        .history_reader()
        .search(
            HistorySearch {
                query: "shared source content".into(),
                ..Default::default()
            },
            &crate::CancelToken::new(),
        )
        .unwrap()
        .hits[0]
        .reference
        .clone();
    reference.seq = None;
    let preview = app
        .history_reader()
        .share(&reference, None, &crate::CancelToken::new())
        .unwrap();
    let selection = SessionShareSelection {
        reference,
        recent_turns: None,
        version: preview.version,
    };
    app.new_session().unwrap();
    let message = app
        .freeze_session_share_message(
            crate::message::PendingMessage::from_front_end(
                "quote request",
                Some("share-delete".into()),
                Vec::new(),
            ),
            std::slice::from_ref(&selection),
            &crate::CancelToken::new(),
        )
        .unwrap();
    let (completion, receiver) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message,
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    receiver
        .recv_timeout(crate::test_support::HANG)
        .unwrap()
        .unwrap();
    let quoter = app.current_session_id().unwrap();
    app.set_session_organization(source.as_str(), None, Some(true), true)
        .unwrap();
    app.new_session().unwrap();
    let error = app.delete_session(source.as_str(), true).unwrap_err();
    assert_eq!(error.presentation().key, "session.delete_share_referenced");
    // 点名引用方：错误文案带引用方会话的标题（首条用户消息派生）。
    assert!(
        error.presentation().english().contains("quote request"),
        "guard must name the referencing session title: {}",
        error.presentation().english()
    );
    // 默认拒绝，不自动解除：引用方仍在，目标仍在列表。
    assert!(
        app.list_sessions()
            .unwrap()
            .iter()
            .any(|s| s.id.as_str() == source.as_str())
    );
    // 解除引用 = 删除引用方（先归档，两段式同守卫）；随后目标可删。
    app.set_session_organization(quoter.as_str(), None, Some(true), true)
        .unwrap();
    app.new_session().unwrap();
    app.delete_session(quoter.as_str(), true).unwrap();
    app.delete_session(source.as_str(), true).unwrap();
    assert!(
        app.list_sessions()
            .unwrap()
            .iter()
            .all(|s| s.id.as_str() != source.as_str())
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
