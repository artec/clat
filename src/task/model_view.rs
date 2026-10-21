//! Bounded model DTOs preserve durable identities before generic result pruning.
use super::{TaskService, TaskState};
use serde_json::{Value, json};
const LIMIT: usize = 4096;
impl TaskService {
    pub(crate) fn model_view(s: &TaskState, args: &Value) -> Value {
        let full = serde_json::to_value(s).expect("task DTO");
        let section = args["section"].as_str().unwrap_or("summary");
        let offset = args["offset"].as_u64().unwrap_or(0).min(128) as usize;
        if section == "summary" && full.to_string().len() <= LIMIT {
            return full;
        }
        let mut view = json!({"id":s.id,"revision":s.revision,"phase":s.phase,"bounded_view":true,"section":section,"offset":offset,
            "budget_remaining":s.remaining(),"operations":[],"acceptance_state":{"reports":[]},
            "read_more":"read_task supports section=operations|reports|contract and offset. User /task show exposes full committed state. These are summaries; verification reads the actual confirmed contract and bytes."});
        let operations:Vec<_>=s.operations.iter().map(|o|json!({"id":o.id,"tool":o.tool,"outcome":o.outcome,
            "command_fact":o.command_fact.as_ref().map(|f|json!({"contract_version":f.contract_version,"clauses":f.clauses,"receipt_status":f.receipt.as_ref().map(|r|&r["status"])}))})).collect();
        let a = s.acceptance_state.as_ref();
        view["contract_version"] = json!(a.and_then(|a| a.contract.as_ref()).map(|c| c.version));
        view["candidate"] = json!(a.and_then(|a| a.candidate.as_ref()));
        if let Some(report) = a.and_then(|a| a.reports.last()) {
            view["acceptance_state"]["reports"] = json!([{"id":report.id,"status":report.status,"contract_version":report.contract_version}]);
        }
        let rows=match section {
            "operations"=>operations,
            "reports"=>a.map(|a|a.reports.iter().flat_map(|r|r.results.iter().map(move|x|json!({"report_id":r.id,"report_status":r.status,"contract_version":r.contract_version,"clause":x.clause,"status":x.status,"source":x.source,"reason":x.reason.chars().take(128).collect::<String>()}))).collect()).unwrap_or_default(),
            "contract"=>a.and_then(|a|a.contract.as_ref()).map(|c|c.clauses.iter().map(|p|json!({"id":p.id,"required":p.required,"checker":p.checker,"checker_version":p.checker_version,"source":p.source,"files":p.files.iter().map(|p|p.chars().take(64).collect::<String>()).collect::<Vec<_>>(),"paths_may_be_shortened":true,
                "parameters_digest":super::state::digest(p.parameters.to_string().as_bytes()),"parameters":"Confirmed full rule is in task recovery context and user /task show; this page omits its potentially large literals."})).collect()).unwrap_or_default(),
            _=>{
                view["operations"]=json!(operations.iter().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>());
                Vec::new()
            }
        };
        if section != "summary" {
            paginate(&mut view, section, rows, offset);
        }
        while view.to_string().len() > LIMIT {
            let key = if section == "summary" {
                "operations"
            } else {
                "items"
            };
            let Some(array) = view[key].as_array_mut() else {
                break;
            };
            if array.is_empty() {
                break;
            }
            array.remove(0);
            view["additional_rows_omitted"] = json!(true);
        }
        view
    }
}
fn paginate(view: &mut Value, section: &str, rows: Vec<Value>, offset: usize) {
    let mut page = Vec::new();
    let mut next = offset;
    for row in rows.iter().skip(offset).take(4) {
        page.push(row.clone());
        view["items"] = json!(page);
        if view.to_string().len() > LIMIT - 256 {
            page.pop();
            break;
        }
        next += 1;
    }
    view["items"] = json!(page);
    view["total"] = json!(rows.len());
    view["next_offset"] = json!((next < rows.len() && next > offset).then_some(next));
    if section == "operations" {
        view["operations"] = view["items"].clone();
        view.as_object_mut().unwrap().remove("items");
    }
}
