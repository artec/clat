//! Exact and approximate observations for process tools.
use serde_json::Value;
use std::hash::{DefaultHasher, Hash, Hasher};

pub(super) struct CommandEvidence {
    pub exact: Option<u64>,
    pub probe: Option<u64>,
}

pub(super) fn observe(name: &str, args: &Value, output: &Value) -> CommandEvidence {
    let result = output_stamp(output);
    let cwd = output["cwd"]
        .as_str()
        .or_else(|| args["cwd"].as_str())
        .or_else(|| args["workdir"].as_str())
        .unwrap_or(".");
    let exact = result.map(|result| stamp(("command", name, args.to_string(), cwd, result)));
    let probe = probe_scopes(args, cwd).map(|scopes| stamp(("read-probe", scopes, result)));
    CommandEvidence { exact, probe }
}

fn stamp(value: impl Hash) -> u64 {
    let mut hash = DefaultHasher::new();
    value.hash(&mut hash);
    hash.finish()
}

fn output_stamp(output: &Value) -> Option<u64> {
    if [
        "running",
        "output_omitted_from_journal",
        "output_truncated",
        "stdout_truncated",
        "stderr_truncated",
        "pty_truncated",
        "stdout_lossy",
        "stderr_lossy",
        "pty_lossy",
    ]
    .iter()
    .any(|key| output[*key] == true)
    {
        return None;
    }
    let streams = ["stdout", "stderr", "pty"].map(|key| output[key].as_str());
    let error = output["error"].as_str();
    if error.is_none()
        && (streams.iter().all(Option::is_none)
            || ["exit_code", "signal"]
                .iter()
                .all(|key| output[*key].is_null()))
    {
        return None;
    }
    let status = [
        "exit_code",
        "signal",
        "timed_out",
        "cancelled",
        "terminated",
        "sandbox",
        "verification_receipt",
    ]
    .map(|key| output.get(key).map(Value::to_string));
    Some(stamp((streams, error, status)))
}

/// Group all read targets in the command. A coarse match can only be advisory.
fn probe_scopes(args: &Value, cwd: &str) -> Option<Vec<String>> {
    if let Some(argv) = args["argv"].as_array() {
        let parts: Vec<_> = argv.iter().map(Value::as_str).collect::<Option<_>>()?;
        if argv_has_heavy_primary(&parts) || read_binary_can_mutate(&parts) {
            return None;
        }
        return Some(vec![format!(
            "{}@{}",
            fingerprint_read_tokens(&parts)?,
            scrub_volatile(cwd)
        )]);
    }
    let cmd = args["cmd"].as_str().or_else(|| args["command"].as_str())?;
    if shell_command_has_heavy_primary(cmd) {
        return None;
    }
    let mut cwd = cwd.to_owned();
    let mut scopes = Vec::new();
    for (step, primary) in split_shell_steps(cmd)? {
        let tokens = tokenize_shell_segment(primary);
        let Some(&first) = tokens.first() else {
            continue;
        };
        if has_stdout_file_redirect(step)
            || read_binary_can_mutate(&tokens)
            || step
                .split_whitespace()
                .any(|word| matches!(word, "tee" | "xargs"))
        {
            return None;
        }
        match first {
            "cd" => {
                let dir = primary[2..].trim().trim_matches(['\'', '"']);
                if dir.is_empty() {
                    return None;
                }
                cwd = if dir.starts_with('/') {
                    dir.to_owned()
                } else {
                    format!("{cwd}/{dir}")
                };
            }
            "echo" | "printf" | "date" | ":" => {}
            _ if shell_assignment(primary, &tokens) => {}
            _ => scopes.push(format!(
                "{}@{}",
                fingerprint_read_tokens(&tokens)?,
                scrub_volatile(&cwd)
            )),
        }
    }
    if scopes.is_empty() {
        return None;
    }
    scopes.sort_unstable();
    scopes.dedup();
    Some(scopes)
}

fn shell_assignment(primary: &str, tokens: &[&str]) -> bool {
    primary.split_once('=').is_some_and(|(name, value)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            && (tokens.len() == 1 || (value.starts_with("$(") && value.ends_with(')')))
    })
}

fn read_binary_can_mutate(tokens: &[&str]) -> bool {
    match tokens.first().map(|bin| binary_basename(bin)) {
        Some("sed") => tokens.iter().skip(1).any(|token| {
            token.starts_with("-i") || *token == "--in-place" || token.starts_with("--in-place=")
        }),
        Some("find") => tokens
            .iter()
            .skip(1)
            .any(|token| matches!(*token, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir")),
        _ => false,
    }
}

/// The heuristic only needs top-level steps and the primary command of a pipeline.
/// Quoted/escaped line breaks and command substitutions stay inside their step.
fn split_shell_steps(cmd: &str) -> Option<Vec<(&str, &str)>> {
    let mut steps = Vec::new();
    let mut start = 0;
    let mut first_pipe = None;
    let mut quote = None;
    let mut parens = 0u32;
    let mut chars = cmd.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '\\' && quote != Some('\'') {
            chars.next();
            continue;
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            continue;
        }
        let next = chars.peek().map(|(_, ch)| *ch);
        match ch {
            '(' => parens = parens.checked_add(1)?,
            ')' => parens = parens.checked_sub(1)?,
            '<' if next == Some('<') => return None, // Here-documents are opaque to this heuristic.
            _ => {}
        }
        if parens != 0 || ch == ')' {
            continue;
        }
        if ch == '#' && (i == start || cmd[..i].ends_with(char::is_whitespace)) {
            push_step(&mut steps, cmd, start, i, first_pipe.take());
            start = chars
                .find(|(_, c)| *c == '\n')
                .map_or(cmd.len(), |(i, _)| i + 1);
            continue;
        }
        if ch == '|' && !matches!(next, Some('|')) {
            first_pipe.get_or_insert(i);
            continue;
        }
        let width = match (ch, next) {
            ('&', Some('&')) | ('|', Some('|')) => {
                chars.next();
                2
            }
            ('\n', _) if cmd[start..i].trim_end().ends_with('|') => continue,
            (';' | '\n', _) => 1,
            _ => continue,
        };
        push_step(&mut steps, cmd, start, i, first_pipe.take());
        start = i + width;
    }
    if quote.is_some() || parens != 0 {
        return None;
    }
    push_step(&mut steps, cmd, start, cmd.len(), first_pipe);
    Some(steps)
}

fn push_step<'a>(
    steps: &mut Vec<(&'a str, &'a str)>,
    cmd: &'a str,
    start: usize,
    end: usize,
    first_pipe: Option<usize>,
) {
    let step = cmd[start..end].trim();
    if !step.is_empty() {
        steps.push((step, cmd[start..first_pipe.unwrap_or(end)].trim()));
    }
}

fn shell_command_has_heavy_primary(cmd: &str) -> bool {
    // Scan every token: live probes often hide `curl` inside `p(){ ...; head; }`
    // bodies after `;`, so primary-of-segment alone misses them.
    cmd.split_whitespace().any(|token| {
        let cleaned = token
            .trim_matches(|c: char| matches!(c, '"' | '\'' | '(' | ')' | '{' | '}' | '`' | ','));
        is_heavy_shell_binary(binary_basename(cleaned))
    })
}

fn argv_has_heavy_primary(parts: &[&str]) -> bool {
    parts
        .iter()
        .any(|part| is_heavy_shell_binary(binary_basename(part)))
}

fn is_heavy_shell_binary(bin: &str) -> bool {
    matches!(
        bin,
        "curl"
            | "wget"
            | "cargo"
            | "rustc"
            | "python"
            | "python3"
            | "node"
            | "npm"
            | "pnpm"
            | "yarn"
            | "bun"
            | "bash"
            | "sh"
            | "zsh"
            | "fish"
            | "ruby"
            | "perl"
            | "php"
            | "go"
            | "make"
            | "cmake"
            | "docker"
            | "podman"
    )
}

/// Detect file redirection while allowing common diagnostic redirects.
fn has_stdout_file_redirect(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'>' {
            i += 1;
            continue;
        }
        let prev = i.checked_sub(1).and_then(|idx| bytes.get(idx).copied());
        if prev == Some(b'>') {
            i += 1;
            continue;
        }
        if prev == Some(b'&') {
            i += 1;
            continue;
        }
        if prev.is_some_and(|c| c.is_ascii_digit()) {
            // `2>/dev/null`, `1>&2` — not a stdout write by cat/tee.
            i += 1;
            continue;
        }
        return true;
    }
    false
}

fn fingerprint_read_tokens(tokens: &[&str]) -> Option<String> {
    let mut idx = 0;
    while idx < tokens.len() {
        let token = tokens[idx];
        if token == "cd" {
            idx += 2;
            continue;
        }
        if token.contains('=') && !token.starts_with('-') && !token.contains('/') {
            idx += 1;
            continue;
        }
        break;
    }
    if idx >= tokens.len() {
        return None;
    }
    let bin = binary_basename(tokens[idx]);
    let (family, rest) = if bin == "git" {
        let sub = *tokens.get(idx + 1)?;
        if !matches!(
            sub,
            "grep" | "show" | "log" | "diff" | "ls-files" | "blame" | "status"
        ) {
            return None;
        }
        (format!("git-{sub}"), idx + 2)
    } else if is_shell_read_binary(bin) {
        (bin.to_owned(), idx + 1)
    } else {
        return None;
    };
    let mut paths: Vec<_> = tokens[rest..]
        .iter()
        .filter_map(|token| path_scope_token(token))
        .map(|path| scrub_volatile(&path))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    Some(format!("{family}|{paths:?}"))
}

fn is_shell_read_binary(bin: &str) -> bool {
    matches!(
        bin,
        "grep"
            | "egrep"
            | "fgrep"
            | "rg"
            | "ag"
            | "ack"
            | "find"
            | "sed"
            | "head"
            | "tail"
            | "cat"
            | "ls"
            | "tree"
            | "wc"
            | "stat"
            | "file"
            | "nm"
            | "strings"
            | "basename"
            | "dirname"
            | "realpath"
            | "readlink"
    )
}

fn binary_basename(token: &str) -> &str {
    token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .split('\\')
        .next_back()
        .unwrap_or(token)
}

fn path_scope_token(token: &str) -> Option<String> {
    let trimmed = token.trim_matches(|c| c == '"' || c == '\'');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "." || trimmed == ".." {
        return Some(".".to_owned());
    }
    if trimmed.starts_with('-') {
        return None;
    }
    let known_root = matches!(
        trimmed,
        "src" | "docs" | "crates" | "locales" | "scripts" | "web" | "desktop" | "sdk" | "tests"
    );
    let has_sep = trimmed.contains('/') || trimmed.contains('\\');
    // Require a separator or known root so grep patterns like `foo.bar` are
    // not mistaken for path scopes. Keep the full path so two files under
    // the same root remain distinct scopes.
    if !known_root && !has_sep {
        return None;
    }
    Some(trimmed.replace('\\', "/"))
}

fn tokenize_shell_segment(segment: &str) -> Vec<&str> {
    segment.split_whitespace().collect()
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replace CLAT run-dir ids and UUID-shaped tokens so re-probes of the same
/// command shape are not treated as novel evidence.
fn scrub_volatile(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while !rest.is_empty() {
        if let Some(idx) = rest.find("clat-run-") {
            out.push_str(&rest[..idx]);
            out.push_str("clat-run-<id>");
            rest = &rest[idx + "clat-run-".len()..];
            let skip = rest
                .find(|c: char| !c.is_ascii_hexdigit() && c != '-')
                .unwrap_or(rest.len());
            rest = &rest[skip..];
            continue;
        }
        if let Some((idx, token_len)) = next_uuid_token(rest) {
            out.push_str(&rest[..idx]);
            out.push_str("<uuid>");
            rest = &rest[idx + token_len..];
            continue;
        }
        out.push_str(rest);
        break;
    }
    collapse_whitespace(&out)
}

fn next_uuid_token(value: &str) -> Option<(usize, usize)> {
    const UUID_LEN: usize = 36;
    let bytes = value.as_bytes();
    let mut i = 0;
    while i + UUID_LEN <= bytes.len() {
        if looks_like_uuid(&bytes[i..i + UUID_LEN]) {
            return Some((i, UUID_LEN));
        }
        i += 1;
    }
    None
}

fn looks_like_uuid(bytes: &[u8]) -> bool {
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => true,
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shell_evidence_splits_only_top_level_shell_steps() {
        let cmd = "cd /repo\n# separate steps\nrg 'one;\ntwo|three' src/main.rs |\n  head -5\nls \\\n src";
        let steps = split_shell_steps(cmd).unwrap();
        let primary: Vec<_> = steps.into_iter().map(|(_, primary)| primary).collect();
        assert_eq!(
            primary,
            [
                "cd /repo",
                "rg 'one;\ntwo|three' src/main.rs",
                "ls \\\n src"
            ]
        );
        assert!(probe_scopes(&json!({"cmd": cmd}), ".").is_some());
    }

    #[test]
    fn shell_evidence_keeps_verification_and_mutation_out_of_read_probes() {
        for args in [
            json!({"cmd":"cd /repo\nrg query src/main.rs\ncargo test | tail -10"}),
            json!({"cmd":"cd /repo\ncat src/main.rs > copy.txt\nls"}),
            json!({"cmd":"cd /repo\nrg query src/main.rs | tee copy.txt"}),
            json!({"cmd":"cat <<'END'\nrg is input data\nEND"}),
            json!({"argv":["find", "src", "-delete"]}),
        ] {
            assert!(probe_scopes(&args, ".").is_none(), "{args}");
        }
    }
}
