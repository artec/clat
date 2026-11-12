//! R21: DSH 0.2.2-rc.1(钉靶 `66491b05e3`)无父 fork 的读者侧行为。
//! 金样由钉靶 SessionStore 真实 fork 路径铸造(gen-r21-fork-fixtures.mts):
//! ① 无父 seeded fork(继承前缀含**指名父会话**的投递确认——上游警告
//! 老 V4 读者会以 "delivery marker names the wrong Session" 拒载);
//! ② 正常顶层对照。裁定(2026-11-11,方案 b):seeded V4 只读打开。
use super::tests::{key_for, load_golden, mount_fixture_generation};
use crate::session::persistence::JsonlCompression;
use crate::session::use_cases::SessionService;

const FORK: (&str, &str) = (
    "v4-fork-seeded-0.2.2.jsonl.zstd",
    "018f7b21-r21d-7000-8000-000000000002",
);
const TOP_LEVEL: (&str, &str) = (
    "v4-top-level-0.2.2.jsonl.zstd",
    "018f7b21-r21d-7000-8000-000000000001",
);
const PARENT_ID: &str = "018f7b21-r21d-7000-8000-000000000001";

/// CLAT 不做投递标记归属校验(`delivery-accepted` 是 Skip 座位),含
/// 错名标记的无父 fork 必须照常装载——上游指南警告的拒载源在 CLAT
/// 不存在,金样把这一点钉死。
#[test]
fn dsh_022_parentless_fork_golden_loads_with_inherited_delivery_marker() {
    let (root, backend) = mount_fixture_generation(FORK.0, FORK.1, 4);
    let events = load_golden(&backend, FORK.1);
    crate::test_support::cleanup_tree(&root);
    assert_eq!(events.len(), 12, "6 inherited + seed marker + 5 fork-owned");
    let marker = events
        .iter()
        .find(|event| event.event_type == "session-log-deepseek/delivery-accepted")
        .expect("inherited delivery marker survives");
    assert_eq!(
        marker.data["sessionId"], PARENT_ID,
        "marker names the parent"
    );
    assert_eq!(events[6].event_type, "session/end-seed");
    assert_eq!(events[6].data["inherited"], true);
}

/// R21-C3 首红腿:经真实 resume 路径打开无父 fork——现状放行为可写,
/// 断言"必须只读 + 续写拒绝 + 零 artifact"在修前红。
#[test]
fn dsh_022_parentless_fork_resumes_read_only_without_artifacts() {
    let (root, _) = mount_fixture_generation(FORK.0, FORK.1, 4);
    let key = key_for(FORK.1);
    let log = crate::session::path_layout::log_path(
        &root,
        Some("/Users/deng/Documents/GitHub/clat"),
        &key.id,
        JsonlCompression::Zstd,
    );
    let bytes = std::fs::read(&log).expect("DSH fork source bytes");
    let service = SessionService::new(root.clone(), JsonlCompression::Zstd).expect("service");
    let view = service
        .resume(&key)
        .expect("parentless seeded fork loads and replays");
    assert!(
        !view.replay.is_empty(),
        "inherited prefix and fork turn both replay"
    );
    fn replay_text(item: &crate::session::replay::ReplayEvent) -> Option<&str> {
        match item {
            crate::session::replay::ReplayEvent::UserMessage { text, .. } => Some(text),
            crate::session::replay::ReplayEvent::AssistantMessage { text, .. } => Some(text),
            _ => None,
        }
    }
    assert!(
        view.replay.iter().filter_map(replay_text).any(|text| {
            text.contains("parent turn before fork") || text.contains("fork continues on its own")
        }),
        "both the inherited parent turn and the fork-owned turn are visible"
    );
    assert!(service.is_read_only(), "seeded V4 opens read-only");
    assert!(
        !service.is_legacy_read_only(),
        "current V4 fork is not legacy: the reason is seeded, not generation"
    );
    let error = match service.journal() {
        Ok(_) => panic!("continuation of a seeded fork must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "This session is a DSH fork. CLAT opened it read-only; continue it in DSH.",
        "actionable, DSH-pointing refusal"
    );
    service.quiesce_active().expect("close read-only session");
    assert_eq!(
        std::fs::read(&log).unwrap(),
        bytes,
        "source bytes untouched"
    );
    assert_eq!(
        std::fs::read_dir(log.parent().unwrap()).unwrap().count(),
        1,
        "no lease, seed, checkpoint or repair artifacts"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 回归面:普通非 seeded V4 行为零变化——对照会话照常可写 resume,
/// resume 本身零追加。
#[test]
fn dsh_022_top_level_control_resumes_writable_unchanged() {
    let (root, _) = mount_fixture_generation(TOP_LEVEL.0, TOP_LEVEL.1, 4);
    let key = key_for(TOP_LEVEL.1);
    let service = SessionService::new(root.clone(), JsonlCompression::Zstd).expect("service");
    service.resume(&key).expect("normal V4 resumes writable");
    assert!(
        !service.is_read_only(),
        "normal V4 stays writable: zero change"
    );
    // 判别面:与 fork 相反,普通 V4 拿得到写 journal(取得写路的
    // 既有行为会补 seed marker——不属 R21 回归面,不断言字节)。
    assert!(service.journal().is_ok());
    service.quiesce_active().expect("close writer");
    let _ = std::fs::remove_dir_all(root);
}

/// 撕裂尾判别腿:最后 zstd 帧物理截断的 seeded fork 仍只读打开——
/// 修复/写路不得接手(租约、修复提交都是 artifact)。该测试在删除
/// Err(Io) 臂 seeded 守卫时红(变异判别)。
#[test]
fn dsh_022_torn_tail_seeded_fork_stays_read_only_not_repaired() {
    let (root, _) = mount_fixture_generation(FORK.0, FORK.1, 4);
    let key = key_for(FORK.1);
    let log = crate::session::path_layout::log_path(
        &root,
        Some("/Users/deng/Documents/GitHub/clat"),
        &key.id,
        JsonlCompression::Zstd,
    );
    let original = std::fs::read(&log).expect("fork source");
    // 物理撕裂:砍掉尾部 20 字节(最后 zstd 帧失效,头帧完好)。
    let torn = &original[..original.len() - 20];
    std::fs::write(&log, torn).expect("tear the tail");
    let service = SessionService::new(root.clone(), JsonlCompression::Zstd).expect("service");
    service
        .resume(&key)
        .expect("torn seeded fork still opens read-only");
    assert!(
        service.is_read_only(),
        "torn tail must not hand the fork to repair"
    );
    assert!(!service.is_legacy_read_only());
    assert!(service.journal().is_err());
    service.quiesce_active().expect("close");
    // 源字节保持撕裂原样(未被修复重写),目录里也只有这一份日志。
    assert_eq!(std::fs::read(&log).unwrap(), torn, "no repair rewrite");
    assert_eq!(
        std::fs::read_dir(log.parent().unwrap()).unwrap().count(),
        1,
        "no lease or repair artifacts"
    );
    let _ = std::fs::remove_dir_all(root);
}
