use super::*;
use std::io::Read;

pub(super) fn child(parent: &Dir, name: &str) -> io::Result<Dir> {
    match parent.create_dir(name) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let dir = crate::session::root_dir::SessionRootDir::open_child(parent, Path::new(name))?;
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        dir.set_permissions(".", cap_std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

fn name(session: &str, turn: u64) -> String {
    format!("{}-{turn}.json", digest(session.as_bytes()))
}

impl FileReview {
    pub(super) fn save(&self, ledger: &Ledger) -> io::Result<()> {
        let text = serde_json::to_string(ledger).map_err(io_error)?;
        if text.len() > RECORD_BYTES {
            return Err(io_error("native recovery record limit reached"));
        }
        crate::private_fs::write_text_atomic_in_dir(
            &self.dir,
            &name(&ledger.session, ledger.turn),
            &text,
        )
        .map_err(io_error)
    }

    pub fn load(&self, session: &str, turn: u64) -> io::Result<Option<Ledger>> {
        let filename = name(session, turn);
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000);
        }
        let file = match self.dir.open_with(&filename, &options) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let meta = file.metadata()?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(io_error("invalid recovery file"));
        }
        let mut bytes = Vec::new();
        file.take((RECORD_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > RECORD_BYTES {
            return Err(io_error("recovery record exceeds limit"));
        }
        let ledger: Ledger = serde_json::from_slice(&bytes).map_err(io_error)?;
        if ledger.version != 1
            || ledger.session != session
            || ledger.turn != turn
            || ledger.root != self.root
            || ledger.files.len() > FILES
        {
            return Err(io_error("recovery identity/version mismatch"));
        }
        for entry in &ledger.files {
            super::super::validate_relative_path(Path::new(&entry.path))?;
            if entry.before.as_ref().is_some_and(|s| s.len() > FILE_BYTES)
                || entry.after.len() > FILE_BYTES
                || entry.previous_capture.as_ref().is_some_and(|c| {
                    c.after.len() > FILE_BYTES || c.stamp.hash != digest(c.after.as_bytes())
                })
            {
                return Err(io_error("recovery file exceeds limit"));
            }
        }
        Ok(Some(ledger))
    }
}
