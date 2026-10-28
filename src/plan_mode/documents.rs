//! Versioned plan facts. Markdown is a projection, never an approval authority.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) const EVENT: &str = "clat/plan/change";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanDocument {
    pub id: String,
    pub session: String,
    pub revision: u64,
    pub title: String,
    pub text: String,
    pub digest: String,
    pub approved: bool,
    pub goal_id: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Documents {
    pub version: u64,
    pub plans: Vec<PlanDocument>,
    pub operations: BTreeMap<String, Operation>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    pub digest: String,
    pub result: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Change {
    pub version: u64,
    pub document: Option<PlanDocument>,
    pub operation: Option<(String, Operation)>,
}
pub(crate) fn decode(value: &Value) -> Result<Change, String> {
    let change: Change = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    if change.version == 0 || change.document.is_none() && change.operation.is_none() {
        return Err("invalid plan change".into());
    }
    if let Some(doc) = &change.document {
        super::validate_plan_text(&doc.text)?;
        if !doc.id.starts_with("plan-")
            || uuid::Uuid::parse_str(&doc.id[5..]).is_err()
            || doc.session.is_empty()
            || doc.revision == 0
            || doc.title.trim().is_empty()
            || doc.title.len() > 512
            || doc.digest != super::plan_digest(&doc.text)
            || doc.goal_id.as_ref().is_some_and(|goal| {
                goal.strip_prefix("goal-")
                    .is_none_or(|id| uuid::Uuid::parse_str(id).is_err())
            })
        {
            return Err("invalid plan document identity or digest".into());
        }
    }
    if let Some((id, operation)) = &change.operation
        && (id.is_empty() || id.len() > 200 || operation.digest.is_empty())
    {
        return Err("invalid workflow operation".into());
    }
    Ok(change)
}
pub(crate) fn fold(state: &mut Documents, value: &Value) -> Result<(), String> {
    let change = decode(value)?;
    if change.version != state.version + 1 {
        return Err("plan revision drift".into());
    }
    if let Some(doc) = change.document {
        if let Some(previous) = state
            .plans
            .iter_mut()
            .find(|p| p.id == doc.id && p.revision == doc.revision)
        {
            if previous.text != doc.text
                || previous.session != doc.session
                || previous.title != doc.title
                || previous.approved && !doc.approved
                || previous.goal_id.is_some() && previous.goal_id != doc.goal_id
            {
                return Err("approved plan revision is immutable".into());
            }
            *previous = doc;
        } else {
            let latest = state
                .plans
                .iter()
                .filter(|p| p.id == doc.id)
                .map(|p| p.revision)
                .max()
                .unwrap_or(0);
            if doc.revision != latest + 1 || doc.approved || doc.goal_id.is_some() {
                return Err("new plan revisions must start as drafts".into());
            }
            state.plans.push(doc);
        }
    }
    if let Some((id, operation)) = change.operation {
        if state.operations.contains_key(&id) {
            return Err("duplicate workflow operation".into());
        }
        state.operations.insert(id, operation);
    }
    state.version = change.version;
    Ok(())
}

pub(crate) fn validate_snapshot(state: &Documents) -> Result<(), String> {
    let mut revisions = BTreeMap::<&str, u64>::new();
    let mut session = None;
    for doc in &state.plans {
        let change = Change {
            version: 1,
            document: Some(doc.clone()),
            operation: None,
        };
        decode(&serde_json::to_value(change).map_err(|e| e.to_string())?)?;
        if session.is_some_and(|id| id != doc.session) {
            return Err("plan checkpoint crosses sessions".into());
        }
        session = Some(doc.session.as_str());
        let expected = revisions.entry(&doc.id).or_default();
        if doc.revision != *expected + 1 {
            return Err("plan checkpoint has missing or duplicate revisions".into());
        }
        *expected = doc.revision;
        if doc.goal_id.is_some() && !doc.approved {
            return Err("unapproved plan cannot own goal execution".into());
        }
    }
    for (id, op) in &state.operations {
        decode(
            &serde_json::to_value(Change {
                version: 1,
                document: None,
                operation: Some((id.clone(), op.clone())),
            })
            .map_err(|e| e.to_string())?,
        )?;
    }
    if state.version < (state.plans.len() + state.operations.len()) as u64 {
        return Err("plan checkpoint version regressed".into());
    }
    Ok(())
}
