use crate::command::{CommandError, CommandOutcome};
use crate::task::{
    TaskService,
    plugin::{failed, identity, pretty},
};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
pub(crate) fn tools(service: Arc<TaskService>) -> Vec<Arc<dyn Tool>> {
    (0..3)
        .map(|kind| {
            Arc::new(AcceptanceTool {
                service: service.clone(),
                kind,
            }) as Arc<dyn Tool>
        })
        .collect()
}
struct AcceptanceTool {
    service: Arc<TaskService>,
    kind: u8,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyRequest {
    task_id: String,
    expected_revision: u64,
    #[serde(default)]
    evidence: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateRequest {
    task_id: String,
    expected_revision: u64,
    report_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckRequest {
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    evidence: BTreeMap<String, String>,
}
impl Tool for AcceptanceTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description, effect, schema) = match self.kind {
            0 => (
                "check_task_acceptance",
                "Read-only bounded checks of user-confirmed task clauses. Evidence maps clause id to actual task operation id; arbitrary JSON is not execution evidence. Does not save results or execute commands.",
                ToolEffect::Read,
                json!({"type":"object","properties":{"offset":{"type":"integer","minimum":0,"maximum":16},"evidence":{"type":"object","maxProperties":16,"additionalProperties":{"type":"string"}}},"additionalProperties":false}),
            ),
            1 => (
                "verify_task_acceptance",
                "Compute and durably save a task verification report under the user-confirmed contract. Requires current task revision; no commands are executed. Missing/failed evidence remains inconclusive/failed. Returns updated state with report ids.",
                ToolEffect::SessionWrite,
                json!({"type":"object","properties":{"task_id":{"type":"string"},"expected_revision":{"type":"integer"},"evidence":{"type":"object","maxProperties":16,"additionalProperties":{"type":"string"}}},"required":["task_id","expected_revision"],"additionalProperties":false}),
            ),
            _ => (
                "propose_task_completion",
                "Submit only a PENDING completion candidate referring to a previously durable passing report. Cannot activate/change contracts, grant human attestations or override failure. Core rechecks evidence after successful Run cleanup/accounting/journal commit; automatic acceptance requires prior explicit user authorization, otherwise user confirmation remains necessary.",
                ToolEffect::SessionWrite,
                json!({"type":"object","properties":{"task_id":{"type":"string"},"expected_revision":{"type":"integer"},"report_id":{"type":"string"}},"required":["task_id","expected_revision","report_id"],"additionalProperties":false}),
            ),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            effect,
            input_schema: schema,
            strict: false,
        }
    }
    fn invoke(&self, args: &Value, _: &Project, cancel: &CancelToken) -> Result<Value, ToolError> {
        if cancel.is_cancelled() || args.to_string().len() > 16384 {
            return Err(ToolError::new(
                "acceptance request cancelled or exceeds bound",
            ));
        }
        let s = self
            .service
            .current()
            .map_err(ToolError::new)?
            .ok_or_else(|| ToolError::new("no task"))?;
        self.service
            .require_binding(&s.id)
            .map_err(ToolError::new)?;
        let value = match self.kind {
            0 => {
                let r: CheckRequest = serde_json::from_value(args.clone())
                    .map_err(|_| ToolError::new("invalid check request"))?;
                if r.offset > 16 {
                    return Err(ToolError::new("invalid check page offset"));
                }
                serde_json::to_value(
                    self.service
                        .check_acceptance(&r.evidence, cancel)
                        .map_err(ToolError::new)?,
                )
            }
            1 => {
                let r: VerifyRequest = serde_json::from_value(args.clone())
                    .map_err(|_| ToolError::new("invalid verify request"))?;
                serde_json::to_value(
                    self.service
                        .save_verification(
                            &r.task_id,
                            r.expected_revision,
                            &r.evidence,
                            true,
                            cancel,
                        )
                        .map_err(ToolError::new)?,
                )
            }
            _ => {
                let r: CandidateRequest = serde_json::from_value(args.clone())
                    .map_err(|_| ToolError::new("invalid candidate request"))?;
                serde_json::to_value(
                    self.service
                        .propose_completion(&r.task_id, r.expected_revision, &r.report_id)
                        .map_err(ToolError::new)?,
                )
            }
        };
        let value = value.map_err(|_| ToolError::new("acceptance response encoding failed"))?;
        if self.kind == 0 {
            return Ok(bounded_report(
                value,
                args["offset"].as_u64().unwrap_or(0).min(16) as usize,
            ));
        }
        let state: crate::task::TaskState = serde_json::from_value(value)
            .map_err(|_| ToolError::new("invalid internal acceptance state"))?;
        Ok(TaskService::model_view(&state, &json!({})))
    }
    fn journal_arguments(&self, args: &Value) -> Value {
        json!({"task_id":args["task_id"].as_str().filter(|s|s.len()<=80),"expected_revision":args["expected_revision"],"report_id":args["report_id"],"request_sha256":crate::task::state::digest(args.to_string().as_bytes()),"parameters_omitted":true})
    }
}
pub(crate) fn command(
    app: &mut crate::application::TrustedProjectApplication,
    verb: &str,
    rest: &str,
) -> Option<Result<CommandOutcome, CommandError>> {
    if !matches!(
        verb,
        "acceptance"
            | "attest"
            | "verify"
            | "verify-save"
            | "complete-verified"
            | "override"
            | "recheck"
            | "verification-status"
    ) {
        return None;
    }
    Some(dispatch(app, verb, rest))
}
fn dispatch(
    app: &mut crate::application::TrustedProjectApplication,
    verb: &str,
    rest: &str,
) -> Result<CommandOutcome, CommandError> {
    if verb == "verify" {
        let evidence = parse_evidence(rest)?;
        return app
            .task_verify(&evidence)
            .map(|r| CommandOutcome::ShowTask(pretty(&r)))
            .map_err(failed);
    }
    if verb == "verification-status" {
        return app
            .task_verification_status((!rest.is_empty()).then_some(rest))
            .map(|r| CommandOutcome::ShowTask(pretty(&r)))
            .map_err(failed);
    }
    let (id, rev, body) = identity(rest)?;
    let state = match verb {
        "acceptance" => {
            app.task_set_acceptance(id, rev, serde_json::from_str(body).map_err(failed)?)
        }
        "attest" => {
            let (clause, note) = body
                .split_once(' ')
                .ok_or_else(|| failed("attest needs a clause and user note"))?;
            app.task_attest(id, rev, clause, note)
        }
        "verify-save" => app.task_save_verification(id, rev, &parse_evidence(body)?),
        "complete-verified" => app.task_complete_verified(id, rev, body),
        "override" => app.task_complete_override(id, rev, body),
        "recheck" => app.task_authorize_recheck(id, rev, body),
        _ => return Err(failed("unknown acceptance command")),
    }
    .map_err(failed)?;
    Ok(CommandOutcome::ShowTask(pretty(&state)))
}
fn parse_evidence(body: &str) -> Result<BTreeMap<String, String>, CommandError> {
    if body.is_empty() {
        Ok(BTreeMap::new())
    } else {
        serde_json::from_str(body).map_err(failed)
    }
}

fn bounded_report(value: Value, offset: usize) -> Value {
    if offset == 0 && value.to_string().len() <= 4096 {
        return value;
    }
    let rows = value["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let page:Vec<_>=rows.iter().skip(offset).take(4).map(|r|json!({"clause":r["clause"],"status":r["status"],"source":r["source"],"reason":r["reason"].as_str().unwrap_or("").chars().take(128).collect::<String>()})).collect();
    json!({"id":value["id"],"task_id":value["task_id"],"contract_version":value["contract_version"],"status":value["status"],"results":page,"total":rows.len(),"offset":offset,"next_offset":(offset+4<rows.len()).then_some(offset+4),"bounded_view":true,"scope":"read-only recomputation; save via verify_task_acceptance for durable digests/provenance"})
}
