//! Frozen revision walks and structured historical file comparisons.
use super::*;
use command::query;
impl GitService {
    pub(super) fn history(&self, value: &Value) -> Result<Value, String> {
        let legacy = value.get("scope").is_none() && value.get("cursor").is_none();
        let cursor = value.get("cursor").filter(|v| !v.is_null());
        let anchors = if let Some(c) = cursor {
            c["anchors"].as_array().ok_or("invalid_cursor")?.clone()
        } else {
            self.history_anchors(if legacy {
                "current"
            } else {
                value["scope"].as_str().unwrap_or("all")
            })?
        };
        if anchors.len() > 4096 {
            return Err("git_output_large".into());
        }
        let skip = cursor
            .and_then(|c| c["skip"].as_u64())
            .unwrap_or_else(|| value["skip"].as_u64().unwrap_or(0));
        if skip > 100000 {
            return Err("git_output_large".into());
        }
        if anchors.is_empty() {
            return Ok(json!({"commits":[],"hasMore":false,"next":0,"nextCursor":null}));
        }
        let mut args = vec![
            "log".into(),
            "-n".into(),
            "51".into(),
            format!("--skip={skip}"),
            "--format=%H%x00%an%x00%aI%x00%s%x00%P%x00%D".into(),
        ];
        if !legacy {
            args.push("--topo-order".into());
        }
        for id in &anchors {
            args.push(oid(id.as_str().ok_or("invalid_cursor")?)?.into());
        }
        args.push("--".into());
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let raw = query(&self.project, &refs)?;
        let mut rows = String::from_utf8(raw).map_err(|_|"git_encoding")?.lines().filter_map(|line| {
            let p = line.splitn(6,'\0').collect::<Vec<_>>();
            (p.len()==6).then(|| json!({"id":p[0],"author":p[1],"date":p[2],"subject":p[3],"parents":p[4].split_whitespace().collect::<Vec<_>>(),"refs":p[5].split(", ").filter(|r|!r.is_empty()).collect::<Vec<_>>()}))
        }).collect::<Vec<_>>();
        let more = rows.len() > 50;
        rows.truncate(50);
        Ok(
            json!({"commits":rows,"hasMore":more,"next":skip+50,"nextCursor":if more {json!({"anchors":anchors,"skip":skip+50})}else{Value::Null}}),
        )
    }
    fn history_anchors(&self, scope: &str) -> Result<Vec<Value>, String> {
        if !["all", "current"].contains(&scope) {
            return Err("invalid_scope".into());
        }
        // Resolve and peel all reference tips in one Git process, without walking
        // their ancestors. Labels are not separate revisions: thousands of tags
        // at one commit must not exhaust a per-label timer or cursor capacity.
        let mut args = vec!["rev-list", "--no-walk=unsorted", "--ignore-missing"];
        if scope == "all" {
            args.extend(["--branches", "--remotes", "--tags"]);
        }
        args.extend(["HEAD", "--"]);
        let raw = String::from_utf8(query(&self.project, &args)?).map_err(|_| "git_encoding")?;
        let mut anchors = raw
            .lines()
            .map(|id| oid(id).map(|id| json!(id)))
            .collect::<Result<Vec<_>, _>>()?;
        anchors.sort_by_key(Value::to_string);
        anchors.dedup();
        Ok(anchors)
    }
    fn comparison(&self, value: &Value) -> Result<(String, Option<String>), String> {
        let commit = oid(text(value, "commit")?)?.to_owned();
        let raw = query(&self.project, &["show", "-s", "--format=%P", &commit, "--"])?;
        let parents = String::from_utf8(raw).map_err(|_| "git_encoding")?;
        let parent = value["parent"]
            .as_str()
            .map(oid)
            .transpose()?
            .map(str::to_owned)
            .or_else(|| parents.split_whitespace().next().map(str::to_owned));
        if parent
            .as_ref()
            .is_some_and(|p| !parents.split_whitespace().any(|x| x == p))
        {
            return Err("invalid_parent".into());
        }
        Ok((commit, parent))
    }
    pub(super) fn commit_files(&self, value: &Value) -> Result<Value, String> {
        let (commit, parent) = self.comparison(value)?;
        let mut args = vec![
            "diff-tree",
            "--no-commit-id",
            "-r",
            "--name-status",
            "-z",
            "-M",
            "--relative",
        ];
        if let Some(parent) = &parent {
            args.push(parent);
        } else {
            args.push("--root");
        }
        args.extend([commit.as_str(), "--", "."]);
        let raw = query(&self.project, &args)?;
        let mut parts = raw.split(|b| *b == 0).filter(|p| !p.is_empty());
        let mut files = Vec::new();
        while let Some(status) = parts.next() {
            let status = std::str::from_utf8(status).map_err(|_| "git_encoding")?;
            let first = std::str::from_utf8(parts.next().ok_or("git_status_invalid")?)
                .map_err(|_| "git_encoding")?;
            let path = if status.starts_with(['R', 'C']) {
                std::str::from_utf8(parts.next().ok_or("git_status_invalid")?)
                    .map_err(|_| "git_encoding")?
            } else {
                first
            };
            relative(path)?;
            relative(first)?;
            files.push(json!({"path":path,"previousPath":first,"status":status}));
        }
        Ok(json!({"commit":commit,"parent":parent,"files":files}))
    }
    pub(super) fn historical_diff(&self, value: &Value) -> Result<Value, String> {
        let files = self.commit_files(value)?;
        let path = text(value, "path")?;
        let row = files["files"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["path"] == path))
            .ok_or("file_missing")?;
        let previous = text(row, "previousPath")?;
        let prefix = query(&self.project, &["rev-parse", "--show-prefix"])?;
        let prefix = String::from_utf8(prefix).map_err(|_| "git_encoding")?;
        let commit = text(&files, "commit")?;
        let parent = files["parent"].as_str();
        let left = parent
            .map(|p| self.history_blob(p, &format!("{}{previous}", prefix.trim_end())))
            .transpose()?
            .unwrap_or_default();
        let right = self.history_blob(commit, &format!("{}{path}", prefix.trim_end()))?;
        let mut result = query::text_diff(path, left, right, parent.unwrap_or("empty"), commit)?;
        result["previousPath"] = json!(previous);
        result["commit"] = json!(commit);
        result["parent"] = json!(parent);
        for id in [parent, Some(commit)].into_iter().flatten() {
            let raw = query(&self.project, &["ls-tree", id, "--", path])?;
            if raw.starts_with(b"160000 ") {
                result["unsupported"] = json!("submodule");
            }
        }
        Ok(result)
    }
    fn history_blob(&self, id: &str, path: &str) -> Result<Vec<u8>, String> {
        let object = format!("{id}:{path}");
        if query(&self.project, &["cat-file", "-e", &object]).is_err() {
            return Ok(Vec::new());
        }
        query(
            &self.project,
            &["show", "--no-ext-diff", "--no-textconv", &object],
        )
    }
}
fn oid(id: &str) -> Result<&str, String> {
    if ![40, 64].contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid_commit".into());
    }
    Ok(id)
}
