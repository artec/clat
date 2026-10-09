"""External behavioral oracles; candidate production items stay byte exact."""
import re


def item(text, anchor):
    start = text.index(anchor)
    opening = text.index("{", start)
    # Strings/comments cannot terminate a Rust item. Supported items are pinned;
    # a changed grammar must fail infrastructure, never be accepted as a bug red.
    tokens = re.compile(r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])\'|[{}]', re.S)
    depth = 0
    for token in tokens.finditer(text, opening):
        if token.group() == "{":
            depth += 1
        elif token.group() == "}":
            depth -= 1
            if depth == 0:
                return text[start:token.end()]
    raise ValueError(f"unterminated production item: {anchor}")


def usage(text):
    return "#[derive(Debug, Default, PartialEq)]\n" + item(text, "pub struct Usage")


PRELUDE = """
#![allow(dead_code, unused_imports)]
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::path::{Path, PathBuf};
use std::io::{Read, Write};
use serde_json::{Value, json};
const SESSION_ID_CAP: usize = 4096;
"""

ORACLES = {
    "ledger": """
let ledger = RunSpendLedger::new(Some(10000));
ledger.reserve(400); ledger.reconcile(120);
assert_eq!(ledger.used(), 120, "ORACLE_normal_usage_replaces_reservation");
ledger.reconcile(u64::MAX);
assert_eq!(ledger.used(), u64::MAX, "ORACLE_hostile_usage_cannot_unspend");
assert!(ledger.exceeds_cap(), "ORACLE_hard_stop_remains_armed");
""",
    "usage": """
let mut normal = Usage {input_tokens: 31, output_tokens: 7,
cached_input_tokens: Some(11), reasoning_tokens: None};
normal.add_assign(&Usage {input_tokens: 5, output_tokens: 3,
cached_input_tokens: Some(2), reasoning_tokens: Some(0)});
assert_eq!(normal.input_tokens, 36, "ORACLE_normal_input_sum");
assert_eq!(normal.output_tokens, 10, "ORACLE_normal_output_sum");
assert_eq!(normal.cached_input_tokens, Some(13), "ORACLE_normal_cache_sum");
assert_eq!(normal.reasoning_tokens, Some(0), "ORACLE_present_zero_not_missing");
normal.add_assign(&Usage::default());
assert_eq!(normal.cached_input_tokens, Some(13), "ORACLE_missing_rhs_preserves_lhs");
assert_eq!(normal.reasoning_tokens, Some(0), "ORACLE_zero_survives_missing_rhs");
let mut empty = Usage::default();
empty.add_assign(&Usage {input_tokens: 0, output_tokens: 0,
cached_input_tokens: Some(0), reasoning_tokens: Some(9)});
assert_eq!(empty.cached_input_tokens, Some(0), "ORACLE_missing_lhs_accepts_zero");
assert_eq!(empty.reasoning_tokens, Some(9), "ORACLE_missing_lhs_accepts_count");
assert_eq!(empty.input_tokens, 0, "ORACLE_zero_input_sum");
assert_eq!(empty.output_tokens, 0, "ORACLE_zero_output_sum");
empty.reasoning_tokens = Some(u64::MAX-1);
let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
empty.add_assign(&Usage {reasoning_tokens: Some(5), ..Usage::default()}); }));
assert!(result.is_ok(), "ORACLE_reasoning_must_not_panic");
assert_eq!(empty.reasoning_tokens, Some(u64::MAX), "ORACLE_reasoning_saturates");
let mut usage = Usage {input_tokens: u64::MAX-1, output_tokens: u64::MAX-1,
cached_input_tokens: Some(u64::MAX-1), reasoning_tokens: None};
let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
usage.add_assign(&Usage {input_tokens: 5, output_tokens: 6,
cached_input_tokens: Some(9), reasoning_tokens: None}); }));
assert!(result.is_ok(), "ORACLE_usage_must_not_panic");
assert_eq!(usage.input_tokens, u64::MAX, "ORACLE_input_saturates");
assert_eq!(usage.output_tokens, u64::MAX, "ORACLE_output_saturates");
assert_eq!(usage.cached_input_tokens, Some(u64::MAX), "ORACLE_optional_saturates");
assert_eq!(usage.reasoning_tokens, None, "ORACLE_missing_not_zero");
""",
    "responses-invalid": """
for value in [json!({"input_tokens":-1,"output_tokens":0}),
json!({"input_tokens":1,"output_tokens":"0"}), json!({"input_tokens":1})] {
assert!(provider::parse_usage(Some(&value)).is_none(), "ORACLE_invalid_usage_unknown");
}
let valid = provider::parse_usage(Some(&json!({"input_tokens":0,"output_tokens":0}))).unwrap();
assert_eq!(valid.input_tokens, 0, "ORACLE_valid_zero_supported");
assert_eq!(valid.output_tokens, 0, "ORACLE_valid_zero_output_supported");
let normal = provider::parse_usage(Some(&json!({"input_tokens":31,"output_tokens":7,
"input_tokens_details":{"cached_tokens":11},"output_tokens_details":{"reasoning_tokens":3}}))).unwrap();
assert_eq!(normal.input_tokens, 31, "ORACLE_admitted_input_preserved");
assert_eq!(normal.output_tokens, 7, "ORACLE_admitted_output_preserved");
assert_eq!(normal.cached_input_tokens, Some(11), "ORACLE_admitted_cache_preserved");
assert_eq!(normal.reasoning_tokens, Some(3), "ORACLE_admitted_reasoning_preserved");
""",
    "responses-clamp": """
let normal = provider::parse_usage(Some(&json!({"input_tokens":31,"output_tokens":7,
"input_tokens_details":{"cached_tokens":11},"output_tokens_details":{"reasoning_tokens":3}}))).unwrap();
assert_eq!(normal.input_tokens, 31, "ORACLE_normal_input_preserved");
assert_eq!(normal.output_tokens, 7, "ORACLE_normal_output_preserved");
assert_eq!(normal.cached_input_tokens, Some(11), "ORACLE_normal_cache_preserved");
assert_eq!(normal.reasoning_tokens, Some(3), "ORACLE_normal_reasoning_preserved");
let zero = provider::parse_usage(Some(&json!({"input_tokens":0,"output_tokens":0}))).unwrap();
assert_eq!(zero.input_tokens, 0, "ORACLE_zero_preserved");
assert_eq!(zero.cached_input_tokens, None, "ORACLE_missing_cache_not_invented");
let usage = provider::parse_usage(Some(&json!({"input_tokens":u64::MAX,
"output_tokens":u64::MAX,"input_tokens_details":{"cached_tokens":u64::MAX},
"output_tokens_details":{"reasoning_tokens":u64::MAX}}))).unwrap();
assert_eq!(usage.input_tokens, 1<<40, "ORACLE_input_bounded");
assert_eq!(usage.output_tokens, 1<<40, "ORACLE_output_bounded");
assert_eq!(usage.cached_input_tokens, Some(1<<40), "ORACLE_cache_bounded");
assert_eq!(usage.reasoning_tokens, Some(1<<40), "ORACLE_reasoning_bounded");
""",
    "compatible": """
for cache in [json!({"prompt_tokens_details":{"cached_tokens":5}}),
json!({"prompt_cache_hit_tokens":5}), json!({"cached_tokens":5})] {
let mut value = cache;
value["prompt_tokens"]=json!(31); value["completion_tokens"]=json!(7);
let normal = provider::parse_usage(&value).unwrap();
assert_eq!(normal.input_tokens, 31, "ORACLE_normal_input_preserved");
assert_eq!(normal.output_tokens, 7, "ORACLE_normal_output_preserved");
assert_eq!(normal.cached_input_tokens, Some(5), "ORACLE_normal_cache_preserved");
assert_eq!(normal.reasoning_tokens, None, "ORACLE_missing_reasoning_preserved");
}
let zero = provider::parse_usage(&json!({"prompt_tokens":0,"completion_tokens":0})).unwrap();
assert_eq!(zero.input_tokens, 0, "ORACLE_zero_preserved");
assert_eq!(zero.cached_input_tokens, None, "ORACLE_missing_cache_preserved");
for cache in [json!({"prompt_tokens_details":{"cached_tokens":u64::MAX}}),
json!({"prompt_cache_hit_tokens":u64::MAX}), json!({"cached_tokens":u64::MAX})] {
let mut value = cache;
value["prompt_tokens"]=json!(u64::MAX); value["completion_tokens"]=json!(u64::MAX);
let usage = provider::parse_usage(&value).unwrap();
assert_eq!(usage.input_tokens, 1<<40, "ORACLE_compatible_input_bounded");
assert_eq!(usage.output_tokens, 1<<40, "ORACLE_compatible_output_bounded");
assert_eq!(usage.cached_input_tokens, Some(1<<40), "ORACLE_all_cache_fallbacks_bounded");
}
""",
    "cancel-ancestor": """
let independent = CancelToken::new();
let leaf = independent.child_with_deadline(Instant::now()+Duration::from_secs(30));
leaf.cancel();
assert!(!independent.is_cancelled(), "ORACLE_child_cannot_cancel_parent");
let parent = CancelToken::new();
let child = parent.child_with_deadline(Instant::now()+Duration::from_secs(20));
let grandchild = child.child_with_deadline(Instant::now()+Duration::from_secs(30));
assert!(!grandchild.is_cancelled(), "ORACLE_live_before_cancel");
parent.cancel();
assert!(grandchild.is_cancelled(), "ORACLE_ancestor_cancellation_propagates");
""",
    "cancel-deadline": """
assert_eq!(CancelToken::new().remaining(), None, "ORACLE_no_deadline_not_invented");
let soon = Instant::now()+Duration::from_secs(10);
let future = CancelToken::with_deadline(soon);
let child = future.child_with_deadline(soon+Duration::from_secs(20));
let grandchild = child.child_with_deadline(soon+Duration::from_secs(30));
assert!(!grandchild.is_cancelled(), "ORACLE_future_ancestor_is_live");
let remaining = grandchild.remaining().unwrap();
assert!(remaining >= Duration::from_secs(9) && remaining <= Duration::from_secs(10),
"ORACLE_live_ancestor_limits_provider_timeout");
let distant = CancelToken::with_deadline(Instant::now()+Duration::from_secs(60));
let short = distant.child_with_deadline(Instant::now()+Duration::from_secs(5));
let remaining = short.remaining().unwrap();
assert!(remaining >= Duration::from_secs(4) && remaining <= Duration::from_secs(5),
"ORACLE_own_shorter_deadline_preserved");
let parent = CancelToken::with_deadline(Instant::now()-Duration::from_secs(1));
let child = parent.child_with_deadline(Instant::now()+Duration::from_secs(20));
let grandchild = child.child_with_deadline(Instant::now()+Duration::from_secs(30));
assert!(grandchild.is_cancelled(), "ORACLE_expired_ancestor_cancels_descendant");
assert_eq!(grandchild.remaining(), Some(Duration::ZERO), "ORACLE_request_deadline_inherits_shortest");
""",
    "memo-bounded": """
let path = std::env::current_dir().unwrap().join("memo");
std::fs::write(&path, "normal-session").unwrap();
assert_eq!(memo::read_last_session_at(&path).as_deref(), Some("normal-session"), "ORACLE_normal_read");
std::fs::write(&path, "x".repeat(4096)).unwrap();
assert_eq!(memo::read_last_session_at(&path).unwrap().len(), 4096, "ORACLE_exact_limit_valid");
std::fs::write(&path, "x".repeat(4097)).unwrap();
assert!(memo::read_last_session_at(&path).is_none(), "ORACLE_oversized_state_rejected");
""",
    "memo-symlink-read": """
let root = std::env::current_dir().unwrap();
let path = root.join("memo");
std::fs::write(&path, "normal-session").unwrap();
assert_eq!(memo::read_last_session_at(&path).as_deref(), Some("normal-session"), "ORACLE_normal_read");
std::fs::remove_file(&path).unwrap();
std::fs::write(root.join("victim"), "private-session").unwrap();
std::os::unix::fs::symlink("victim", &path).unwrap();
assert!(memo::read_last_session_at(&path).is_none(), "ORACLE_symlink_not_read");
assert_eq!(std::fs::read_to_string(root.join("victim")).unwrap(), "private-session", "ORACLE_victim_preserved");
""",
    "memo-symlink-write": """
let root = std::env::current_dir().unwrap();
let path = root.join("memo");
memo::remember_last_session_at(&path, "normal");
assert_eq!(std::fs::read_to_string(&path).unwrap(), "normal", "ORACLE_normal_write");
std::fs::remove_file(&path).unwrap();
std::fs::write(root.join("victim"), "private-session").unwrap();
std::os::unix::fs::symlink("victim", &path).unwrap();
memo::remember_last_session_at(&path, "replacement");
assert_eq!(std::fs::read_to_string(root.join("victim")).unwrap(), "private-session", "ORACLE_symlink_victim_preserved");
assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink(), "ORACLE_link_not_replaced");
""",
    "memo-absolute-read": """
let root = std::env::current_dir().unwrap();
let path = root.join("memo");
std::fs::write(&path, "normal-session").unwrap();
assert_eq!(memo::read_last_session_at(&path).as_deref(), Some("normal-session"), "ORACLE_normal_read");
std::fs::remove_file(&path).unwrap();
std::fs::write(root.join("victim"), "private-session").unwrap();
std::os::unix::fs::symlink(root.join("victim"), &path).unwrap();
assert!(memo::read_last_session_at(&path).is_none(), "ORACLE_absolute_symlink_not_read");
""",
}


def mutate(text, kind):
    if kind == "ledger":
        return text.replace("self.saturating_add_used(actual_tokens);", "self.saturating_add_used(actual_tokens.min(1024));")
    if kind == "usage":
        return text.replace("saturating_add", "wrapping_add")
    if kind == "responses-invalid":
        return text.replace('value.get("input_tokens")?.as_u64()?', 'value.get("input_tokens").and_then(Value::as_u64).unwrap_or(0)')
    if kind in ["responses-clamp", "compatible"]:
        return text.replace(".min(super::MAX_USAGE_FIELD_TOKENS)", "")
    if kind == "cancel-ancestor":
        return text.replace("parent.is_cancelled()", "parent.cancelled.load(Ordering::Relaxed)")
    if kind == "cancel-deadline":
        return text.replace("parent.is_cancelled()", "parent.cancelled.load(Ordering::Relaxed)").replace(
            "self.parent.as_ref().and_then(|parent| parent.remaining())", "None::<Duration>")
    if kind == "memo-bounded":
        return text.replace("text.len() > SESSION_ID_CAP", "text.len() > SESSION_ID_CAP + 1")
    if kind in ["memo-symlink-read", "memo-absolute-read"]:
        original = item(text, "pub(crate) fn read_last_session_at")
        return text.replace(original, 'pub(crate) fn read_last_session_at(path: &Path) -> Option<String> { std::fs::read_to_string(path).ok() }')
    if kind == "memo-symlink-write":
        original = item(text, "pub(crate) fn remember_last_session_at")
        return text.replace(original, 'pub(crate) fn remember_last_session_at(path: &Path, session: &str) { let _ = std::fs::write(path, session); }')
    raise ValueError(f"unregistered decoy: {kind}")


def memo_helpers(text):
    helpers = "\n".join(item(text, anchor) for anchor in [
        "pub(crate) fn write_text_atomic", "fn reject_symlink", "fn temp_file_name"])
    return """mod sentinel { pub fn sync_dir(path: &std::path::Path) -> std::io::Result<()> {
std::fs::File::open(path)?.sync_all() } }
mod json_file { use super::*; static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
""" + helpers + "\n}\n"


def source_for(workspace, task, decoy=False):
    kind = task["kind"]
    path = workspace / task["target"]
    text = path.read_text(encoding="utf-8")
    if decoy:
        changed = mutate(text, kind)
        if changed == text:
            raise ValueError(f"decoy did not alter production code: {task['id']}")
        path.write_text(changed, encoding="utf-8")
        text = changed
    if kind == "ledger":
        production = item(text, "pub struct RunSpendLedger") + item(text, "impl RunSpendLedger")
    elif kind == "usage":
        production = usage(text) + item(text, "impl Usage") + item(text, "fn add_optional")
    elif kind.startswith("cancel-"):
        production = "#[derive(Clone, Debug, Default)]\n" + item(text, "pub struct CancelToken") + item(text, "impl CancelToken")
    elif kind.startswith("responses-") or kind == "compatible":
        model = (workspace / "src/model.rs").read_text(encoding="utf-8")
        helpers = (workspace / "src/providers/mod.rs").read_text(encoding="utf-8")
        common = ""
        if "fn clamp_usage_field" in helpers:
            constant = re.search(r"pub\(crate\) const MAX_USAGE_FIELD_TOKENS[^;]+;", helpers).group()
            common = constant + item(helpers, "pub(crate) fn clamp_usage_field")
        function = item(text, "fn parse_usage").replace("fn parse_usage", "pub(super) fn parse_usage", 1)
        production = common + usage(model) + "mod provider {use super::*;" + function + "}"
    elif kind.startswith("memo-"):
        functions = item(text, "pub(crate) fn read_last_session_at")
        if kind == "memo-symlink-write":
            helpers = (workspace / "src/control_storage/json_file.rs").read_text(encoding="utf-8")
            functions += item(text, "pub(crate) fn remember_last_session_at")
            production = memo_helpers(helpers)
        else:
            production = ""
        production += "mod memo {use super::*;" + functions + "}"
    else:
        raise ValueError(f"unknown probe: {kind}")
    return PRELUDE + production + "\n#[test]\nfn external_oracle() {\n" + ORACLES[kind] + "\n}\n"
