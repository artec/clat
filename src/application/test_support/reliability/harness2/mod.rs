//! Private experiment bridge. No runtime configuration or public RPC surface.
use super::*;
use serde::Deserialize;
use std::collections::BTreeMap;
mod audit;
mod editor;
mod failure;
mod tests;
pub(super) mod views;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub role: String,
    pub output: PathBuf,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub trial: usize,
    #[serde(default)]
    pub control: bool,
    pub guidance: Option<String>,
    #[serde(default)]
    pub mission: String,
    #[serde(default)]
    pub evidence: BTreeMap<String, String>,
}

#[test]
#[ignore = "private Harness2 bridge; explicitly arm CLAT_HARNESS2_LIVE; paid requests"]
fn harness2_bridge() {
    if std::env::var("CLAT_HARNESS2_LIVE").as_deref() != Ok("1") {
        eprintln!("Harness2 NOT_ARMED (not an executed acceptance)");
        return;
    }
    // Armed workers accept their manifest on stdin; no key or task body in argv.
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin()
        .take(8 * 1024 * 1024 + 1)
        .read_to_string(&mut input)
        .unwrap();
    assert!(input.len() <= 8 * 1024 * 1024);
    let manifest: Manifest = serde_json::from_str(&input).expect("valid private manifest");
    validate(&manifest).unwrap();
    std::fs::create_dir(&manifest.output).expect("never overwrite earlier evidence");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(&manifest)));
    let mut value = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => failure::record(&manifest, error),
        Err(error) => failure::record(&manifest, panic_text(error)),
    };
    audit::attach(&manifest, &mut value);
    write_new(&manifest.output.join("result.json"), &value);
}

fn validate(m: &Manifest) -> Result<(), String> {
    if !m.output.is_absolute() || m.output.exists() {
        return Err("output must be a fresh absolute directory".into());
    }
    if !matches!(
        m.role.as_str(),
        "execute" | "improve" | "review" | "probe" | "baseline" | "offline" | "corpus"
    ) {
        return Err("unknown bridge role".into());
    }
    if m.role == "execute"
        && (!matches!(m.family.as_str(), "requirements" | "tools" | "images") || m.trial > 100)
    {
        return Err("unknown synthetic task".into());
    }
    if let Some(text) = &m.guidance {
        validate_guidance(text)?;
    }
    Ok(())
}

pub(super) fn validate_guidance(text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > 64 * 1024 || text.contains('\0') {
        return Err("invalid guidance text".into());
    }
    // Eight-section response schema is an invariant, not an editable experiment variable.
    for heading in [
        "Primary Request and Intent",
        "Key Technical Concepts",
        "Files and Code",
        "Errors and Fixes",
        "Pending Jobs",
        "Current Work",
        "Next Step",
        "Critical Context",
    ] {
        if !text.contains(&format!("## {heading}")) {
            return Err("guidance must preserve the eight-section contract".into());
        }
    }
    Ok(())
}

fn dispatch(m: &Manifest) -> Result<Value, String> {
    match m.role.as_str() {
        "offline" => offline(m),
        "baseline" => {
            Ok(json!({"status":"completed","guidance":crate::plugins::harness2_default_guidance()}))
        }
        "corpus" => Ok(corpus_identity()),
        "execute" => exercise::run_observed(
            &corpus::Case {
                family: m.family.clone(),
                trial: m.trial,
                control: m.control,
            },
            &m.output,
            m.guidance.as_deref().map(Arc::from),
            false,
        ),
        "improve" => editor::improve(m),
        "probe" if m.family.ends_with("-tool") => editor::improve(m),
        "review" | "probe" => editor::text_request(m),
        _ => unreachable!(),
    }
}

fn corpus_identity() -> Value {
    use sha2::{Digest, Sha256};
    let mut tasks = BTreeMap::new();
    for family in ["requirements", "tools", "images"] {
        for trial in [0, 1, 2, 20, 21, 22, 30, 31, 32] {
            for control in [false, true] {
                let case = corpus::Case {
                    family: family.into(),
                    trial,
                    control,
                };
                let inputs = json!({"initial":case.variant(case.initial()),"correction":case.variant(case.correction()),
                    "evidence":case.variant(case.evidence()),"recall":case.recall(),"tool_prompt":case.tool_prompt(),
                    "resources":(0..8).map(|id| tools::observation(&case,id,family=="tools")).collect::<Vec<_>>(),
                    "images":{"size":[128,128],"rgb":[[0,0,220],[220,0,0],[0,220,0]],"batches":exercise::IMAGE_BATCHES}});
                tasks.insert(
                    case.id(),
                    format!("{:x}", Sha256::digest(serde_json::to_vec(&inputs).unwrap())),
                );
            }
        }
    }
    json!({"status":"completed","tasks":tasks})
}

fn offline(m: &Manifest) -> Result<Value, String> {
    let root = m.output.join("project");
    std::fs::create_dir(&root).unwrap();
    let storage = m.output.join("state");
    std::fs::create_dir(&storage).unwrap();
    exercise::policy(&storage);
    let project = Project::new(root);
    let mut app = super::super::tests::mount(
        &project,
        &storage,
        crate::test_support::TestBehavior::Success,
    );
    crate::test_support::configure_test_model(&app);
    let result = super::super::tests::run(&mut app, &"source evidence ".repeat(2000))
        .map_err(|e| e.error.to_string())?;
    super::super::tests::run(&mut app, "retained latest user input")
        .map_err(|e| e.error.to_string())?;
    app.compact_session()
        .map_err(|e| e.to_string())?
        .join_report()
        .unwrap()
        .map_err(|e| e.raw)?;
    let id = app.current_session_id().unwrap();
    let before = app.sessions.surface_nodes().unwrap();
    app.close().map_err(|e| e.to_string())?;
    let mut app = super::super::tests::mount(
        &project,
        &storage,
        crate::test_support::TestBehavior::Success,
    );
    app.switch_session(id.clone()).map_err(|e| e.to_string())?;
    let replay = before == app.sessions.surface_nodes().unwrap();
    app.close().map_err(|e| e.to_string())?;
    let evidence = m.output.join("observations");
    std::fs::create_dir(&evidence).unwrap();
    views::export(&m.output.join("state"), &evidence, &id.to_string());
    Ok(
        json!({"status":"completed","text":result.output,"replay_equal":replay,"evidence_root":evidence}),
    )
}
