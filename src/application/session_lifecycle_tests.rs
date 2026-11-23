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

/// 序列敏感测试的确定性夹具：关掉 utility 异步写者（自动命名/摘要/
/// 建议/自动跟进）。X4 家族复发（CI 2026-11-22 两轮盒子，旋转变受害
/// 者）：`TestBehavior::Success` 会真生成标题，title worker 的 journal
/// 写入落在断言窗内——`committed_seq` 漂移 +1，或删除后为被删会话
/// 复写 overview 使目录复活。关设置即可（`maybe_autotitle` 逐任务读
/// 设置），不必逐行为旁路。
fn quiet_utilities(application: &TrustedProjectApplication) {
    use crate::application::UtilitySettingsEdit;
    application
        .edit_utility_settings(UtilitySettingsEdit {
            naming_enabled: false,
            summary_enabled: Some(false),
            suggestions_enabled: false,
            automatic_followups: false,
            profile: None,
        })
        .unwrap();
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
    quiet_utilities(&app);
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
    quiet_utilities(&app);
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
    quiet_utilities(&app);
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

/// INV-SL7：删除依次动三个存储（会话目录 → 恢复账本 → 组织索引），中途
/// 停下的删除必须可以继续。目录已删而组织索引仍标为已归档，正是上一次
/// 删除停在存储之间的残态：重试只做剩余清理并成功，不再移动选择；三处
/// 都没有残留的 id 仍是明确的“不存在”（上测的幂等重删契约不变）。
#[test]
fn delete_resumes_when_an_earlier_delete_stopped_between_its_stores() {
    let (storage, root) = roots("sl7-delete-resume");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    quiet_utilities(&app);
    run(&mut app, "victim session content").unwrap();
    let victim = app.current_session_id().unwrap();
    app.set_session_organization(victim.as_str(), None, Some(true), true)
        .unwrap();
    app.new_session().unwrap();
    run(&mut app, "keeper session").unwrap();
    let keeper = app.current_session_id().unwrap();
    // The state an interrupted delete leaves behind: the directory (first
    // store) is gone while the organization index (last store) still lists
    // the archived id.
    app.sessions
        .remove_session(&app.session_key(&victim))
        .unwrap();
    assert!(
        app.list_sessions()
            .unwrap()
            .iter()
            .all(|s| s.id.as_str() != victim.as_str())
    );
    assert!(
        app.selection_is_archived(&victim).unwrap(),
        "the organization index must still carry the archived id"
    );

    // Before the fix the retry died on "does not exist" and stranded the id.
    assert_eq!(
        app.delete_session(victim.as_str(), true).unwrap(),
        false,
        "a resumed delete never moves the selection"
    );
    assert!(
        !app.selection_is_archived(&victim).unwrap(),
        "the resumed delete must forget the organization index entry"
    );
    assert_eq!(app.current_session_id(), Some(keeper));
    assert!(
        !crate::test_support::storage_tree_mentions(&storage, victim.as_str()),
        "no store may mention the deleted id any more"
    );
    // Nothing is left anywhere, so a further delete stays an explicit not-found.
    assert!(
        app.delete_session(victim.as_str(), true)
            .unwrap_err()
            .to_string()
            .contains("does not exist")
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

/// INV-SL8：删除当前浏览的归档会话时，选择先按 /new 语义腾空，再动存储。
/// 之后某个存储写失败时 Host 其实已经切换——错误必须如实带上“选择已
/// 变更”，各客户端才会重同步；重试完成清理后不再移动选择。
#[test]
fn delete_that_fails_after_vacating_the_selection_reports_that_the_selection_moved() {
    let (storage, root) = roots("sl8-delete-selection-moved");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    quiet_utilities(&app);
    run(&mut app, "browsed archived conversation").unwrap();
    let victim = app.current_session_id().unwrap();
    app.set_session_organization(victim.as_str(), None, Some(true), true)
        .unwrap();
    // Browsing an archived conversation is the normal entry for its delete.
    app.switch_session(victim.clone()).unwrap();
    app.inject_next_session_forget_failure(victim.as_str());

    let error = app.delete_session(victim.as_str(), true).unwrap_err();
    assert!(
        error.selection_changed(),
        "the Host already moved off the deleted conversation: {error}"
    );
    assert!(
        app.current_session_id().is_none(),
        "the selection was vacated to Fresh before the stores were touched"
    );
    // Half done: the directory is gone, the organization index still lists
    // the archived id.
    assert!(app.selection_is_archived(&victim).unwrap());
    // The retry finishes the cleanup without moving the selection again.
    let current = app.current_session_id();
    assert_eq!(app.delete_session(victim.as_str(), true).unwrap(), false);
    assert!(!app.selection_is_archived(&victim).unwrap());
    assert_eq!(app.current_session_id(), current);
    assert!(
        !crate::test_support::storage_tree_mentions(&storage, victim.as_str()),
        "no store may mention the deleted id once the retry succeeds"
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

/// INV-SL7 的边界：续做入口只服务于「目录已经不在」的会话。直接调用它
/// 时，日志仍在磁盘上的会话必须被拒绝——那条会话只是没被列出（例如
/// 头部不可读），绝不能经续做路径被删掉（不消失不变量）。
#[test]
fn interrupted_delete_resume_never_removes_a_conversation_whose_journal_remains() {
    let (storage, root) = roots("sl7-resume-keeps-journal");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    quiet_utilities(&app);
    run(&mut app, "still on disk").unwrap();
    let victim = app.current_session_id().unwrap();
    app.set_session_organization(victim.as_str(), None, Some(true), true)
        .unwrap();
    app.new_session().unwrap();
    let error = app.finish_interrupted_delete(victim.as_str()).unwrap_err();
    assert!(error.to_string().contains("does not exist"), "{error}");
    assert!(
        app.sessions.has_log(&app.session_key(&victim)),
        "the journal must still be on disk"
    );
    assert!(
        app.selection_is_archived(&victim).unwrap(),
        "the organization entry must be untouched"
    );
    assert_eq!(
        app.list_sessions()
            .unwrap()
            .iter()
            .filter(|s| s.id == victim)
            .count(),
        1
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn interrupted_delete_resume_preserves_an_existing_session_directory() {
    let (storage, root) = roots("sl7-resume-keeps-directory");
    std::fs::create_dir_all(&root).unwrap();
    let project = Project::new(&root);
    let mut app = mount(&project, &storage, TestBehavior::Success);
    configure_test_model(&app);
    quiet_utilities(&app);
    run(&mut app, "preserve this archived conversation").unwrap();
    let archived = app.current_session_id().unwrap();
    app.set_session_organization(archived.as_str(), None, Some(true), true)
        .unwrap();
    app.new_session().unwrap();

    let session_dir = crate::session::path_layout::session_dir(
        &storage.join("sessions"),
        app.project_key().header_cwd.as_deref(),
        &archived,
    );
    let journal = session_dir.join(crate::session::compat::log_file_name(
        JsonlCompression::Zstd,
    ));
    let retained = session_dir.join("retained-journal.tmp");
    let original = std::fs::read(&journal).unwrap();
    std::fs::rename(&journal, &retained).unwrap();
    assert!(app.list_sessions().unwrap().is_empty());

    let outcome = app.delete_session(archived.as_str(), true);
    assert!(
        outcome.is_err(),
        "the session directory still exists: {outcome:?}"
    );
    assert_eq!(std::fs::read(&retained).unwrap(), original);
    assert!(app.selection_is_archived(&archived).unwrap());

    std::fs::rename(&retained, &journal).unwrap();
    app.close().unwrap();
    let mut reopened = mount(&project, &storage, TestBehavior::Success);
    assert!(
        reopened
            .list_sessions()
            .unwrap()
            .iter()
            .any(|session| { session.id == archived && session.archived })
    );
    assert!(!reopened.switch_session(archived).unwrap().replay.is_empty());
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
