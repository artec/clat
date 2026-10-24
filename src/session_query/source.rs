use super::{
    index::{State, digest, sql_error},
    *,
};
use crate::session::{catalog::search, event::SurfaceOp, projection::TitleTracker};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::HashSet;
impl SessionQueryService {
    pub(super) fn sync(
        &self,
        connection: &mut Connection,
        state: &mut State,
        projects: &[ProjectKey],
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<(), QueryError> {
        for project in projects {
            check(cancel, deadline)?;
            let rows = self
                .reader
                .list(project)
                .map_err(|e| QueryError::new("source_unavailable").detail(e))?;
            let mut present = HashSet::new();
            for (key, header, revision) in rows {
                check(cancel, deadline)?;
                let id = key_text(&key);
                present.insert(id.clone());
                let cached: Option<String> = connection
                    .query_row("SELECT revision FROM sessions WHERE key=?1", [&id], |r| {
                        r.get(0)
                    })
                    .optional()
                    .map_err(sql_error)?;
                if cached.as_deref() == Some(&revision) {
                    continue;
                }
                self.sync_one(
                    connection,
                    &key,
                    header.version,
                    &revision,
                    cancel,
                    deadline,
                )?;
                state.epoch = state.epoch.wrapping_add(1);
            }
            let mut statement = connection
                .prepare("SELECT key FROM sessions WHERE project=?1")
                .map_err(sql_error)?;
            let old: Vec<String> = statement
                .query_map([project_text(project)], |r| r.get(0))
                .map_err(sql_error)?
                .collect::<rusqlite::Result<_>>()
                .map_err(sql_error)?;
            drop(statement);
            for id in old.into_iter().filter(|key| !present.contains(key)) {
                let tx = connection.transaction().map_err(sql_error)?;
                remove_documents(&tx, &id, None)?;
                tx.execute("DELETE FROM sessions WHERE key=?1", [id])
                    .map_err(sql_error)?;
                tx.commit().map_err(sql_error)?;
                state.epoch = state.epoch.wrapping_add(1);
            }
        }
        Ok(())
    }
    fn sync_one(
        &self,
        connection: &mut Connection,
        key: &SessionKey,
        version: u32,
        revision: &str,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<(), QueryError> {
        let tx = connection.transaction().map_err(sql_error)?;
        let id = key_text(key);
        tx.execute("UPDATE documents SET visible=1 WHERE session_key=?1", [&id])
            .map_err(sql_error)?;
        let mut title = TitleTracker::default();
        let mut last = -1i64;
        let mut floor = -1i64;
        let mut failure = None;
        let read=self.reader.visit(key,&mut |event|{
            let result=(||{
                check(cancel,deadline)?;
                title.push(event).map_err(|e|QueryError::new("source_unavailable").detail(e))?;
                if search::begins_step(event){floor=event.seq as i64;}
                if let Some(SurfaceOp::Replace{start,end})=&event.surface_op
                    && !search::preserves_history(event){tx.execute("UPDATE documents SET visible=0 WHERE session_key=?1 AND seq BETWEEN ?2 AND ?3",params![&id,*start as i64,*end as i64]).map_err(sql_error)?;}
                if let Some(document)=search::document(event){put_document(&tx,&id,event.seq as i64,document.kind,event.time,&document.text)?;}
                else {remove_documents(&tx,&id,Some(event.seq as i64))?;}
                last=event.seq as i64;Ok(())
            })();
            if let Err(error)=result {failure=Some(error);return Err("history query interrupted".into());} Ok(())
        });
        if let Some(error) = failure {
            return Err(error);
        }
        let header = read.map_err(|e| QueryError::new("source_unavailable").detail(e))?;
        if header.version != version
            || self
                .reader
                .revision(key)
                .map_err(|e| QueryError::new("source_unavailable").detail(e))?
                != revision
        {
            return Err(QueryError::new("source_changed"));
        }
        remove_after(&tx, &id, last)?;
        let title = match title.explicit_title() {
            Some(title) => crate::redact::redact_secrets(&title),
            None => {
                let first:Option<String>=tx.query_row("SELECT text FROM documents WHERE session_key=?1 AND role='user' AND visible=1 ORDER BY seq LIMIT 1",[&id],|r|r.get(0)).optional().map_err(sql_error)?;
                crate::session::projection::fallback_title(first.as_deref().unwrap_or(""))
            }
        };
        put_document(&tx, &id, -1, "title", header.created_at, &title)?;
        tx.execute("INSERT INTO sessions(key,project,session,title,revision,last_seq,step_floor) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(key) DO UPDATE SET title=excluded.title,revision=excluded.revision,last_seq=excluded.last_seq,step_floor=excluded.step_floor",
            params![id,project_text(&key.project),key.id.as_str(),title,revision,last,floor]).map_err(sql_error)?;
        tx.commit().map_err(sql_error)
    }
}
fn put_document(
    tx: &Transaction<'_>,
    id: &str,
    seq: i64,
    role: &str,
    time: i64,
    text: &str,
) -> Result<(), QueryError> {
    let hash = digest(&format!("{role}:{time}:{text}"));
    let row:Option<i64>=tx.query_row("INSERT INTO documents(session_key,seq,role,time,text,folded,digest) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(session_key,seq) DO UPDATE SET role=excluded.role,time=excluded.time,text=excluded.text,folded=excluded.folded,digest=excluded.digest WHERE digest!=excluded.digest RETURNING id",
        params![id,seq,role,time,text,text.to_lowercase(),hash],|r|r.get(0)).optional().map_err(sql_error)?;
    if let Some(row) = row {
        tx.execute("DELETE FROM words WHERE rowid=?1", [row])
            .map_err(sql_error)?;
        tx.execute("DELETE FROM trigrams WHERE rowid=?1", [row])
            .map_err(sql_error)?;
        tx.execute(
            "INSERT INTO words(rowid,text) VALUES(?1,?2)",
            params![row, text],
        )
        .map_err(sql_error)?;
        tx.execute(
            "INSERT INTO trigrams(rowid,text) VALUES(?1,?2)",
            params![row, text.to_lowercase()],
        )
        .map_err(sql_error)?;
    }
    Ok(())
}
fn remove_documents(tx: &Transaction<'_>, key: &str, seq: Option<i64>) -> Result<(), QueryError> {
    let filter = if seq.is_some() {
        "session_key=?1 AND seq=?2"
    } else {
        "session_key=?1 AND (?2 IS NULL)"
    };
    for table in ["words", "trigrams"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE rowid IN(SELECT id FROM documents WHERE {filter})"),
            params![key, seq],
        )
        .map_err(sql_error)?;
    }
    tx.execute(
        &format!("DELETE FROM documents WHERE {filter}"),
        params![key, seq],
    )
    .map_err(sql_error)?;
    Ok(())
}
fn remove_after(tx: &Transaction<'_>, key: &str, last: i64) -> Result<(), QueryError> {
    for table in ["words", "trigrams"] {
        tx.execute(&format!("DELETE FROM {table} WHERE rowid IN(SELECT id FROM documents WHERE session_key=?1 AND seq>?2)"),params![key,last]).map_err(sql_error)?;
    }
    tx.execute(
        "DELETE FROM documents WHERE session_key=?1 AND seq>?2",
        params![key, last],
    )
    .map_err(sql_error)?;
    Ok(())
}
