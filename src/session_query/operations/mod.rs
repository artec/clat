//! Read-only tool facts. A cursor freezes a journal prefix, never a writable handle.
mod fold;
mod scan;
use super::*;
use fold::OperationScan;
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OperationFilter {
    pub turn: Option<u64>,
    pub tool: Option<String>,
    pub status: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationsRequest {
    pub reference: HistoryReference,
    #[serde(default)]
    pub filter: OperationFilter,
    pub cursor: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationDetailRequest {
    pub reference: HistoryReference,
    pub id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationRow {
    pub id: String,
    pub seq: u64,
    pub turn: u64,
    pub time: i64,
    pub call_id: String,
    pub tool: String,
    pub target: String,
    pub status: String,
    pub result_seq: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationsPage {
    pub rows: Vec<OperationRow>,
    pub next_cursor: Option<String>,
    pub boundary: Option<u64>,
    pub tools: Vec<String>,
    pub turns: Vec<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationDetail {
    pub row: OperationRow,
    pub arguments: Option<String>,
    pub result: Option<String>,
    pub truncated: bool,
}
#[derive(Serialize, Deserialize)]
struct Cursor {
    reference: HistoryReference,
    filter: OperationFilter,
    boundary: u64,
    digest: String,
    before: u64,
    before_turn: u64,
}
impl SessionQueryService {
    pub(crate) fn operations(
        &self,
        request: OperationsRequest,
        cancel: &CancelToken,
    ) -> Result<OperationsPage, QueryError> {
        let key = self.resolve(&request.reference, false, false)?;
        let cursor = parse_cursor(&request)?;
        let mut facts = OperationScan::new(None);
        let (boundary, digest) = self.scan_operations(&key, cancel, cursor.as_ref(), &mut facts)?;
        let tools = facts.tools();
        let turns = facts.turns();
        let mut rows: Vec<_> = facts
            .rows
            .into_values()
            .filter(|row| matches_filter(row, &request.filter))
            .filter(|row| {
                cursor
                    .as_ref()
                    .is_none_or(|c| (row.turn, row.seq) < (c.before_turn, c.before))
            })
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse((row.turn, row.seq)));
        let more = rows.len() > 40;
        rows.truncate(40);
        let next_cursor = if more {
            Some(
                serde_json::to_string(&Cursor {
                    reference: request.reference,
                    filter: request.filter,
                    boundary: boundary.unwrap(),
                    digest,
                    before: rows.last().unwrap().seq,
                    before_turn: rows.last().unwrap().turn,
                })
                .expect("cursor serializes"),
            )
        } else {
            None
        };
        Ok(OperationsPage {
            rows,
            next_cursor,
            boundary,
            tools,
            turns,
        })
    }
    pub(crate) fn operation_detail(
        &self,
        request: OperationDetailRequest,
        cancel: &CancelToken,
    ) -> Result<OperationDetail, QueryError> {
        let key = self.resolve(&request.reference, false, false)?;
        let mut facts = OperationScan::new(Some(request.id.clone()));
        self.scan_operations(&key, cancel, None, &mut facts)?;
        let row = facts
            .rows
            .into_values()
            .find(|r| r.id == request.id)
            .ok_or_else(|| QueryError::new("not_found"))?;
        Ok(OperationDetail {
            row,
            arguments: facts.arguments,
            result: facts.result,
            truncated: facts.truncated,
        })
    }
}
fn parse_cursor(request: &OperationsRequest) -> Result<Option<Cursor>, QueryError> {
    let Some(text) = &request.cursor else {
        return Ok(None);
    };
    if text.len() > 8192 {
        return Err(QueryError::new("invalid_query"));
    }
    let cursor: Cursor =
        serde_json::from_str(text).map_err(|_| QueryError::new("invalid_query"))?;
    if cursor.reference != request.reference || cursor.filter != request.filter {
        return Err(QueryError::new("scope_changed"));
    }
    Ok(Some(cursor))
}
fn matches_filter(row: &OperationRow, filter: &OperationFilter) -> bool {
    filter.turn.is_none_or(|turn| row.turn == turn)
        && filter.tool.as_ref().is_none_or(|tool| &row.tool == tool)
        && filter
            .status
            .as_ref()
            .is_none_or(|status| &row.status == status)
}

#[cfg(test)]
mod tests;
