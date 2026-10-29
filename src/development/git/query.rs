use super::*;
use command::query;
impl GitService {
    pub(super) fn snapshot(&self) -> Result<Value, String> {
        let status = query(
            &self.project,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--",
                ".",
            ],
        )?;
        let head = String::from_utf8(
            query(&self.project, &["rev-parse", "--verify", "HEAD"]).unwrap_or_default(),
        )
        .map_err(|_| "git_encoding")?
        .trim()
        .to_owned();
        let index = query(&self.project, &["ls-files", "--stage", "-z"])?;
        let branch = String::from_utf8(
            query(
                &self.project,
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
            )
            .unwrap_or_default(),
        )
        .map_err(|_| "git_encoding")?
        .trim()
        .to_owned();
        let prefix = String::from_utf8(query(&self.project, &["rev-parse", "--show-prefix"])?)
            .map_err(|_| "git_encoding")?;
        let files = parse_status(&status, prefix.trim_end())?;
        Ok(
            json!({"head":head,"index":digest(&index),"status":working_version(&self.project,&status,&files)?,"branch":branch,"files":files}),
        )
    }
    pub(super) fn diff(&self, value: &Value) -> Result<Value, String> {
        let path = text(value, "path")?;
        relative(path)?;
        let staged = value["staged"] == true;
        let snapshot = self.snapshot()?;
        let file = snapshot["files"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["path"] == path))
            .ok_or("file_missing")?;
        let previous = file["previousPath"].as_str().unwrap_or(path);
        let index_path = if file["status"]
            .as_str()
            .is_some_and(|status| status.starts_with(['R', 'C']))
        {
            path
        } else {
            previous
        };
        let prefix = String::from_utf8(query(&self.project, &["rev-parse", "--show-prefix"])?)
            .map_err(|_| "git_encoding")?;
        let original = format!("{}{previous}", prefix.trim_end());
        let current = format!("{}{path}", prefix.trim_end());
        let indexed = format!("{}{index_path}", prefix.trim_end());
        let left = if staged {
            self.blob(&format!("HEAD:{original}"))?
        } else {
            self.blob(&format!(":{indexed}"))?
        };
        let right = if staged {
            self.blob(&format!(":{current}"))?
        } else {
            self.project
                .read_file_limited(path, 2 * 1024 * 1024 + 1)
                .map_err(io)?
                .unwrap_or_default()
        };
        text_diff(
            path,
            left,
            right,
            if staged { "HEAD" } else { "index" },
            if staged { "index" } else { "worktree" },
        )
    }
    fn blob(&self, object: &str) -> Result<Vec<u8>, String> {
        if query(&self.project, &["cat-file", "-e", object]).is_err() {
            return Ok(Vec::new());
        }
        query(
            &self.project,
            &["show", "--no-ext-diff", "--no-textconv", object],
        )
    }
    pub(super) fn branches(&self) -> Result<Value, String> {
        let raw = query(
            &self.project,
            &[
                "for-each-ref",
                "--format=%(refname:short)%00%(HEAD)%00%(upstream:short)%00%(refname)",
                "refs/heads/",
                "refs/remotes/",
            ],
        )?;
        let rows = String::from_utf8(raw)
            .map_err(|_| "git_encoding")?
            .lines()
            .map(|line| {
                let p: Vec<_> = line.split('\0').collect();
                json!({"name":p[0],"current":p.get(1)==Some(&"*"),"upstream":p.get(2),"remote":p.get(3).is_some_and(|p|p.starts_with("refs/remotes/"))})
            })
            .collect::<Vec<_>>();
        Ok(json!({"branches":rows}))
    }
    pub(super) fn history(&self, value: &Value) -> Result<Value, String> {
        let skip = value["skip"].as_u64().unwrap_or(0).min(100000);
        let next = skip + 50;
        if query(&self.project, &["rev-parse", "--verify", "HEAD"]).is_err() {
            return Ok(json!({"commits":[],"next":0,"hasMore":false}));
        }
        let skip = format!("--skip={skip}");
        let raw = query(
            &self.project,
            &["log", "-n", "50", &skip, "--format=%H%x00%an%x00%aI%x00%s"],
        )?;
        let rows = String::from_utf8(raw)
            .map_err(|_| "git_encoding")?
            .lines()
            .filter_map(|line| {
                let p: Vec<_> = line.splitn(4, '\0').collect();
                (p.len() == 4).then(|| json!({"id":p[0],"author":p[1],"date":p[2],"subject":p[3]}))
            })
            .collect::<Vec<_>>();
        Ok(json!({"commits":rows,"next":next,"hasMore":rows.len()==50}))
    }
    pub(super) fn commit_detail(&self, value: &Value) -> Result<Value, String> {
        let id = text(value, "commit")?;
        if id.len() != 40 && id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid_commit".into());
        }
        let raw = query(
            &self.project,
            &[
                "show",
                "--no-ext-diff",
                "--no-textconv",
                "--format=fuller",
                id,
                "--",
                ".",
            ],
        )?;
        Ok(json!({"id":id,"text":String::from_utf8_lossy(&raw)}))
    }
}
fn parse_status(bytes: &[u8], prefix: &str) -> Result<Vec<Value>, String> {
    let mut rows = bytes.split(|b| *b == 0).filter(|x| !x.is_empty());
    let mut result = Vec::new();
    while let Some(row) = rows.next() {
        if row.len() < 4 {
            return Err("git_status_invalid".into());
        }
        let status = std::str::from_utf8(&row[..2]).map_err(|_| "git_encoding")?;
        let raw = std::str::from_utf8(&row[3..]).map_err(|_| "git_encoding")?;
        let previous = if status.contains(['R', 'C']) {
            Some(
                std::str::from_utf8(rows.next().ok_or("git_status_invalid")?)
                    .map_err(|_| "git_encoding")?,
            )
        } else {
            None
        };
        let Some(path) = raw.strip_prefix(prefix) else {
            continue;
        };
        relative(path)?;
        result.push(json!({"path":path,"status":status,"staged":row[0]!=b' '&&row[0]!=b'?',"unstaged":row[1]!=b' ',"untracked":status=="??","previousPath":previous.and_then(|p|p.strip_prefix(prefix))}));
    }
    Ok(result)
}
fn text_diff(
    path: &str,
    left: Vec<u8>,
    right: Vec<u8>,
    left_label: &str,
    right_label: &str,
) -> Result<Value, String> {
    let unsupported = left.len() > 2 * 1024 * 1024
        || right.len() > 2 * 1024 * 1024
        || left.contains(&0)
        || right.contains(&0);
    let l = String::from_utf8(left);
    let r = String::from_utf8(right);
    let binary = unsupported || l.is_err() || r.is_err();
    Ok(
        json!({"path":path,"left":l.unwrap_or_default(),"right":r.unwrap_or_default(),"leftLabel":left_label,"rightLabel":right_label,"binary":binary}),
    )
}

fn working_version(project: &Project, status: &[u8], files: &[Value]) -> Result<String, String> {
    let mut bytes = status.to_vec();
    for file in files {
        if file["unstaged"] != true && file["untracked"] != true {
            continue;
        }
        let path = text(file, "path")?;
        let identity = project.fingerprint_file(path, 64 * 1024 * 1024, &CancelToken::new());
        match identity {
            Ok(identity) => bytes.extend_from_slice(identity.sha256.as_bytes()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => bytes.push(0),
            Err(error) => {
                let root = cap_std::fs::Dir::open_ambient_dir(
                    project.root(),
                    cap_std::ambient_authority(),
                )
                .map_err(io)?;
                let meta = root.symlink_metadata(path).map_err(io)?;
                if meta.file_type().is_symlink() {
                    bytes.extend_from_slice(
                        root.read_link(path)
                            .map_err(io)?
                            .to_string_lossy()
                            .as_bytes(),
                    );
                } else if meta.is_dir() {
                    bytes.extend_from_slice(path.as_bytes());
                } else {
                    return Err(io(error));
                }
            }
        }
    }
    Ok(digest(&bytes))
}
