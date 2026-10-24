use super::{
    index::{Cursor, State, digest, sql_error},
    *,
};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
use std::collections::HashSet;
const CANDIDATES: usize = 200;
const PAGE: usize = 20;
struct Candidate {
    hit: HistoryHit,
    score: f64,
}
impl SessionQueryService {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn query(
        &self,
        connection: &Connection,
        state: &mut State,
        projects: &[ProjectKey],
        request: &HistorySearch,
        current: Option<&SessionKey>,
        agent: bool,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<HistoryPage, QueryError> {
        if request.query.trim().is_empty() || request.query.chars().count() > 512 {
            return Err(QueryError::new("invalid_query"));
        }
        let fingerprint = digest(
            &serde_json::to_string(&(
                request.query.as_str(),
                request.trusted_projects,
                &request.session,
                projects.iter().map(project_text).collect::<Vec<_>>(),
                agent,
                current.map(key_text),
            ))
            .unwrap(),
        );
        if let Some(token) = &request.cursor {
            let cursor = state
                .cursors
                .get(token)
                .filter(|c| c.fingerprint == fingerprint && c.epoch == state.epoch)
                .ok_or_else(|| QueryError::new("cursor_expired"))?
                .clone();
            return Ok(page(state, cursor, agent));
        }
        if projects.len() > 256 {
            return Err(QueryError::new("scope_too_large"));
        }
        let (hits, capped) = collect_candidates(
            connection, projects, request, current, agent, cancel, deadline,
        )?;
        Ok(page(
            state,
            Cursor {
                fingerprint,
                epoch: state.epoch,
                offset: 0,
                hits: Arc::new(hits),
                truncated: capped,
            },
            agent,
        ))
    }
}
fn page(state: &mut State, cursor: Cursor, agent: bool) -> HistoryPage {
    let end = (cursor.offset + if agent { 10 } else { PAGE }).min(cursor.hits.len());
    let hits = cursor.hits[cursor.offset..end].to_vec();
    let next_cursor = if end < cursor.hits.len() && !agent {
        if state.cursors.len() >= 8 {
            state.cursors.clear();
        }
        let token = uuid::Uuid::new_v4().to_string();
        state.cursors.insert(
            token.clone(),
            Cursor {
                offset: end,
                ..cursor.clone()
            },
        );
        Some(token)
    } else {
        None
    };
    HistoryPage {
        hits,
        next_cursor,
        truncated: cursor.truncated || (agent && end < cursor.hits.len()),
        index_state: "ready".into(),
    }
}
fn statement(
    engine: &str,
    projects: &[ProjectKey],
    request: &HistorySearch,
    current: Option<&SessionKey>,
    agent: bool,
) -> (String, Vec<SqlValue>) {
    let mut args: Vec<SqlValue> = projects
        .iter()
        .map(|p| SqlValue::Text(project_text(p)))
        .collect();
    let placeholders = (0..args.len()).map(|_| "?").collect::<Vec<_>>().join(",");
    let mut filter = format!("d.visible=1 AND s.project IN({placeholders})");
    if let Some(session) = &request.session {
        filter.push_str(" AND s.session=? AND d.seq>=0");
        args.push(SqlValue::Text(session.clone()));
    }
    if agent && let Some(current) = current {
        if request.session.is_none() {
            filter.push_str(" AND s.key!=?");
            args.push(SqlValue::Text(key_text(current)));
        } else {
            filter.push_str(" AND (s.key!=? OR d.seq<s.step_floor)");
            args.push(SqlValue::Text(key_text(current)));
        }
    }
    let (join, score) = if engine == "short" {
        (String::new(), "0.0")
    } else {
        (
            format!("JOIN {engine} f ON f.rowid=d.id"),
            if engine == "words" {
                "bm25(words)"
            } else {
                "bm25(trigrams)"
            },
        )
    };
    let columns = format!("s.project,s.session,s.title,d.seq,d.role,d.time,d.text,{score}");
    if engine == "short" {
        filter.push_str(" AND instr(d.folded,?)>0");
        args.push(SqlValue::Text(request.query.to_lowercase()));
    } else {
        filter.push_str(&format!(" AND {engine} MATCH ?"));
        let query = if engine == "trigrams" {
            request.query.to_lowercase()
        } else {
            request.query.clone()
        };
        args.push(SqlValue::Text(format!(
            "\"{}\"",
            query.replace('"', "\"\"")
        )));
    }
    (
        format!(
            "SELECT {columns} FROM documents d JOIN sessions s ON s.key=d.session_key {join} WHERE {filter} ORDER BY {} ASC,d.time DESC,d.id LIMIT {CANDIDATES}",
            if engine == "short" { "d.id*0" } else { score }
        ),
        args,
    )
}
fn excerpt(text: &str, query: &str) -> String {
    let lower = text.to_lowercase();
    let needle = query.to_lowercase();
    let byte = lower.find(&needle).unwrap_or(0);
    let start = lower[..byte].chars().count().saturating_sub(60);
    let content: String = text.chars().skip(start).take(240).collect();
    if start > 0 {
        format!("…{content}")
    } else {
        content
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_candidates(
    connection: &Connection,
    projects: &[ProjectKey],
    request: &HistorySearch,
    current: Option<&SessionKey>,
    agent: bool,
    cancel: &CancelToken,
    deadline: Instant,
) -> Result<(Vec<HistoryHit>, bool), QueryError> {
    let mut rows = Vec::new();
    let mut capped = false;
    let engines = if request.query.chars().count() < 3 {
        vec!["short"]
    } else {
        vec!["words", "trigrams"]
    };
    for engine in engines {
        check(cancel, deadline)?;
        let (sql, args) = statement(engine, projects, request, current, agent);
        let mut query = connection.prepare(&sql).map_err(sql_error)?;
        let found: Vec<_> = query
            .query_map(params_from_iter(args), |row| candidate(row, &request.query))
            .map_err(sql_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_error)?;
        capped |= found.len() >= CANDIDATES;
        rows.extend(found);
    }
    rows.sort_by(|a, b| {
        a.score
            .total_cmp(&b.score)
            .then_with(|| b.hit.time.cmp(&a.hit.time))
            .then_with(|| a.hit.reference.session.cmp(&b.hit.reference.session))
            .then_with(|| a.hit.reference.seq.cmp(&b.hit.reference.seq))
    });
    let mut seen = HashSet::new();
    let hits: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            seen.insert(
                serde_json::to_string(&(
                    row.hit.reference.project.as_deref(),
                    &row.hit.reference.session,
                    if request.session.is_some() {
                        row.hit.reference.seq
                    } else {
                        None
                    },
                ))
                .unwrap(),
            )
        })
        .map(|row| row.hit)
        .take(CANDIDATES)
        .collect();

    Ok((hits, capped))
}

fn candidate(row: &rusqlite::Row<'_>, query: &str) -> rusqlite::Result<Candidate> {
    let project: String = row.get(0)?;
    let project: Option<String> = serde_json::from_str(&project).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let seq: i64 = row.get(3)?;
    let body: String = row.get(6)?;
    Ok(Candidate {
        hit: HistoryHit {
            reference: HistoryReference {
                project,
                session: row.get(1)?,
                seq: (seq >= 0).then_some(seq as u64),
            },
            title: row.get(2)?,
            role: row.get(4)?,
            time: row.get(5)?,
            excerpt: excerpt(&body, query),
        },
        score: row.get(7)?,
    })
}
