use super::*;
use crate::session::{
    catalog::search,
    event::{SessionEvent, SurfaceOp},
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEventsRequest {
    pub reference: HistoryReference,
    #[serde(default)]
    pub trusted_projects: bool,
    #[serde(default)]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub event_type: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEventRow {
    pub reference: HistoryReference,
    pub event_type: String,
    pub time: i64,
    pub preview: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEventsPage {
    pub events: Vec<HistoryEventRow>,
    pub next_seq: Option<u64>,
    pub version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryReadRequest {
    pub reference: HistoryReference,
    #[serde(default)]
    pub trusted_projects: bool,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub version: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEventRead {
    pub reference: HistoryReference,
    pub json: String,
    pub next_offset: Option<usize>,
    pub version: String,
}
impl SessionQueryService {
    pub(super) fn resolve(
        &self,
        reference: &HistoryReference,
        all: bool,
        agent: bool,
    ) -> Result<SessionKey, QueryError> {
        if agent && all {
            return Err(QueryError::new("unauthorized"));
        }
        let projects = self.authorized(all);
        let project = projects
            .into_iter()
            .find(|p| p.header_cwd == reference.project)
            .ok_or_else(|| QueryError::new("unauthorized"))?;
        if reference.session.is_empty() || reference.session.len() > 1024 {
            return Err(QueryError::new("invalid_reference"));
        }
        Ok(SessionKey {
            project,
            id: crate::session::id::SessionId::new(&reference.session),
        })
    }
    /// Scan one committed snapshot; never load/repair or select a session.
    pub(super) fn scan(
        &self,
        key: &SessionKey,
        cancel: &CancelToken,
        agent: bool,
        mut visitor: impl FnMut(&SessionEvent),
    ) -> Result<String, QueryError> {
        let deadline = Instant::now() + DEADLINE;
        let revision = self.reader.revision(key).map_err(source_error)?;
        let current = agent && self.reader.current().as_ref() == Some(key);
        let mut floor = u64::MAX;
        let mut hidden = Vec::new();
        let mut failure = None;
        self.reader
            .visit(key, &mut |event| {
                if let Err(e) = check(cancel, deadline) {
                    failure = Some(e);
                    return Err("query interrupted".into());
                }
                if current && search::begins_step(event) {
                    floor = event.seq;
                }
                if let Some(SurfaceOp::Replace { start, end }) = &event.surface_op
                    && !search::preserves_history(event)
                {
                    hidden.push((*start, *end));
                }
                Ok(())
            })
            .map_err(source_error)?;
        if let Some(error) = failure.take() {
            return Err(error);
        }
        self.reader
            .visit(key, &mut |event| {
                if let Err(e) = check(cancel, deadline) {
                    failure = Some(e);
                    return Err("query interrupted".into());
                }
                if event.seq < floor && !hidden.iter().any(|(a, b)| (*a..=*b).contains(&event.seq))
                {
                    // Agent reads public message observations, not reasoning/configuration.
                    if !agent || search::document(event).is_some() {
                        visitor(event);
                    }
                }
                Ok(())
            })
            .map_err(source_error)?;
        if let Some(error) = failure {
            return Err(error);
        }
        if self.reader.revision(key).map_err(source_error)? != revision {
            return Err(QueryError::new("source_changed"));
        }
        if !self.authorized(false).contains(&key.project)
            && !self.authorized(true).contains(&key.project)
        {
            return Err(QueryError::new("scope_changed"));
        }
        check(cancel, deadline)?;
        Ok(index::digest(&revision))
    }
    pub(crate) fn events(
        &self,
        request: HistoryEventsRequest,
        cancel: &CancelToken,
        agent: bool,
    ) -> Result<HistoryEventsPage, QueryError> {
        let key = self.resolve(&request.reference, request.trusted_projects, agent)?;
        let mut events = Vec::new();
        let mut more = false;
        let version = self.scan(&key, cancel, agent, |event| {
            if request.after_seq.is_some_and(|seq| event.seq <= seq)
                || request
                    .event_type
                    .as_ref()
                    .is_some_and(|kind| kind != &event.event_type)
            {
                return;
            }
            if events.len() >= 40 {
                more = true;
                return;
            }
            let preview = search::document(event)
                .map(|d| d.text.chars().take(240).collect())
                .unwrap_or_default();
            events.push(HistoryEventRow {
                reference: HistoryReference {
                    seq: Some(event.seq),
                    ..request.reference.clone()
                },
                event_type: event.event_type.clone(),
                time: event.time,
                preview,
            });
        })?;
        let next_seq = more.then(|| events.last().unwrap().reference.seq.unwrap());
        Ok(HistoryEventsPage {
            events,
            next_seq,
            version,
        })
    }
    pub(crate) fn read(
        &self,
        request: HistoryReadRequest,
        cancel: &CancelToken,
        agent: bool,
    ) -> Result<HistoryEventRead, QueryError> {
        let key = self.resolve(&request.reference, request.trusted_projects, agent)?;
        let seq = request
            .reference
            .seq
            .ok_or_else(|| QueryError::new("invalid_reference"))?;
        let mut value = None;
        self.scan(&key,cancel,agent,|event|{
            if event.seq==seq {
                value=Some(if agent {json!({"seq":seq,"time":event.time,"type":event.event_type,"text":search::document(event).map(|d|d.text)})}
                else {serde_json::to_value(event).expect("event serializes")});
            }
        })?;
        let text = serde_json::to_string_pretty(&redact_value(
            &value.ok_or_else(|| QueryError::new("not_found"))?,
        ))
        .expect("JSON serializes");
        let version = index::digest(&text);
        if request.offset > 0 && request.version.as_deref() != Some(&version) {
            return Err(QueryError::new("source_changed"));
        }
        let total = text.chars().count();
        if request.offset > total {
            return Err(QueryError::new("invalid_reference"));
        }
        let json: String = text.chars().skip(request.offset).take(4096).collect();
        let end = request.offset + json.chars().count();
        Ok(HistoryEventRead {
            reference: request.reference,
            json,
            next_offset: (end < total).then_some(end),
            version,
        })
    }
}
fn source_error(e: impl ToString) -> QueryError {
    QueryError::new("source_unavailable").detail(e)
}
pub(super) fn redact_value(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(crate::redact::redact_secrets(s)),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if crate::redact::credential_name(k) {
                            json!("[REDACTED]")
                        } else {
                            redact_value(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        _ => value.clone(),
    }
}
