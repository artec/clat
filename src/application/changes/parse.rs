use super::*;

pub(super) fn parse_status(bytes: &[u8]) -> Result<Vec<ChangedFile>, String> {
    let mut records = bytes.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mut files = Vec::new();
    while let Some(record) = records.next() {
        if record.len() < 4 || record[2] != b' ' {
            return Err("Invalid Git status record".into());
        }
        let path = std::str::from_utf8(&record[3..])
            .map_err(|_| "Non-UTF-8 Git path cannot be reviewed safely")?
            .to_owned();
        let rename = record[..2].iter().any(|b| matches!(b, b'R' | b'C'));
        let previous_path = if rename {
            Some(
                std::str::from_utf8(records.next().ok_or("Missing rename source")?)
                    .map_err(|_| "Non-UTF-8 rename source")?
                    .to_owned(),
            )
        } else {
            None
        };
        let untracked = &record[..2] == b"??";
        files.push(ChangedFile {
            path,
            previous_path,
            status: String::from_utf8_lossy(&record[..2]).into_owned(),
            staged: !untracked && record[0] != b' ',
            unstaged: untracked || record[1] != b' ',
            untracked,
        });
    }
    Ok(files)
}
