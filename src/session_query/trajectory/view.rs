use super::*;
impl SessionQueryService {
    pub(crate) fn trajectory(
        &self,
        request: TrajectoryRequest,
        cancel: &CancelToken,
    ) -> Result<TrajectoryPage, QueryError> {
        if request.query.len() > 2048 {
            return Err(QueryError::new("invalid_query"));
        }
        let cursor = decode_cursor(&request)?;
        let expected = cursor
            .as_ref()
            .map(|c| &c.snapshot)
            .or(request.snapshot.as_ref());
        let (snapshot, projection) = self.trajectory_scan(
            &request.reference,
            request.trusted_projects,
            expected,
            cancel,
            Projection::new(request.query.trim(), None),
        )?;
        let entries = projection.entries;
        let matched: Vec<_> = entries
            .iter()
            .filter(|e| e.matches)
            .map(|e| e.row.id.clone())
            .collect();
        let anchor = request.anchor_seq.or(request.reference.seq);
        let selected = select(&entries, anchor, !request.query.trim().is_empty());
        let offset = cursor.as_ref().map(|c| c.offset).unwrap_or_else(|| {
            selected
                .map_or(0, |i| i.saturating_sub(PAGE / 3))
                .min(entries.len().saturating_sub(PAGE))
        });
        if offset > entries.len() {
            return Err(QueryError::new("invalid_query"));
        }
        let end = (offset + PAGE).min(entries.len());
        let encode = |offset| {
            serde_json::to_string(&Cursor {
                reference: request.reference.clone(),
                trusted_projects: request.trusted_projects,
                query: request.query.clone(),
                snapshot: snapshot.clone(),
                offset,
            })
            .expect("cursor serializes")
        };
        let page = TrajectoryPage {
            turns: turns(&entries, &projection.steps),
            overview: entries.iter().map(|e| span(&e.row)).collect(),
            records: entries[offset..end].iter().map(|e| e.row.clone()).collect(),
            before_cursor: (offset > 0).then(|| encode(offset.saturating_sub(PAGE))),
            after_cursor: (end < entries.len()).then(|| encode(end)),
            selected_id: selected.map(|i| entries[i].row.id.clone()),
            matches: matched,
            snapshot,
        };
        Ok(page)
    }
    pub(crate) fn trajectory_detail(
        &self,
        request: TrajectoryDetailRequest,
        cancel: &CancelToken,
    ) -> Result<TrajectoryDetail, QueryError> {
        if request.id.len() > 128 {
            return Err(QueryError::new("invalid_reference"));
        }
        let (_, projection) = self.trajectory_scan(
            &request.reference,
            request.trusted_projects,
            Some(&request.snapshot),
            cancel,
            Projection::new("", Some(request.id)),
        )?;
        projection
            .entries
            .into_iter()
            .find_map(|e| e.detail)
            .ok_or_else(|| QueryError::new("not_found"))
    }
}
fn decode_cursor(request: &TrajectoryRequest) -> Result<Option<Cursor>, QueryError> {
    let Some(raw) = &request.cursor else {
        return Ok(None);
    };
    if raw.len() > 8192 {
        return Err(QueryError::new("invalid_query"));
    }
    let cursor: Cursor = serde_json::from_str(raw).map_err(|_| QueryError::new("invalid_query"))?;
    if cursor.reference != request.reference
        || cursor.trusted_projects != request.trusted_projects
        || cursor.query != request.query
        || request
            .snapshot
            .as_ref()
            .is_some_and(|s| s != &cursor.snapshot)
    {
        return Err(QueryError::new("scope_changed"));
    }
    Ok(Some(cursor))
}
fn select(entries: &[Entry], anchor: Option<u64>, searching: bool) -> Option<usize> {
    if let Some(seq) = anchor {
        return entries
            .iter()
            .position(|e| e.row.source_seqs.contains(&seq) && e.row.kind != "reasoning")
            .or_else(|| {
                entries
                    .iter()
                    .position(|e| e.row.source_seqs.contains(&seq))
            });
    }
    if searching {
        return entries.iter().position(|e| e.matches);
    }
    let latest_turn = entries.iter().rev().find_map(|e| e.row.turn);
    entries
        .iter()
        .rposition(|e| e.row.turn == latest_turn && e.row.final_answer)
        .or_else(|| entries.len().checked_sub(1))
}
fn span(row: &Record) -> Span {
    Span {
        id: row.id.clone(),
        seq: row.seq,
        turn: row.turn,
        kind: row.kind.clone(),
        time: row.time,
        started_at: row.started_at,
        ended_at: row.ended_at,
        estimated: row.estimated,
    }
}
fn turns(entries: &[Entry], steps: &BTreeMap<(u64, u64), i64>) -> Vec<Turn> {
    let mut step_counts = BTreeMap::new();
    for &(turn, _) in steps.keys() {
        *step_counts.entry(turn).or_insert(0) += 1;
    }
    let mut rows = Vec::<Turn>::new();
    let mut positions = BTreeMap::new();
    for entry in entries {
        let row = &entry.row;
        let id = row
            .turn
            .map(|t| format!("turn:{t}"))
            .unwrap_or_else(|| row.id.clone());
        let index = *positions.entry(id.clone()).or_insert_with(|| {
            rows.push(Turn {
                id,
                turn: row.turn,
                first_seq: row.seq,
                question: String::new(),
                records: 0,
                steps: row.turn.and_then(|turn| step_counts.get(&turn).copied()),
                tools: 0,
                status: None,
                final_id: None,
            });
            rows.len() - 1
        });
        let group = &mut rows[index];
        group.records += 1;
        group.tools += usize::from(entry.is_call);
        if group.question.is_empty() && row.kind == "user" {
            group.question.clone_from(&row.preview);
        }
        if row.final_answer {
            group.final_id = Some(row.id.clone());
        }
        if row.kind == "terminal" {
            group.status.clone_from(&row.status);
        }
    }
    rows
}
