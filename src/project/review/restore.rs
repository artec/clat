use super::*;
use crate::project::{Project, WritableTarget, WriteGuard};
use std::io::Write;

pub(crate) fn stamp(content: &str, metadata: &cap_std::fs::Metadata) -> io::Result<Stamp> {
    let modified = format!("{:?}", metadata.modified()?.into_std());
    #[cfg(unix)]
    let (identity, mode) = {
        use cap_std::fs::{MetadataExt, PermissionsExt};
        (
            format!("{}:{}", metadata.dev(), metadata.ino()),
            Some(metadata.permissions().mode()),
        )
    };
    #[cfg(not(unix))]
    let (identity, mode) = (format!("{:?}", metadata.created()?.into_std()), None);
    Ok(Stamp {
        hash: digest(content.as_bytes()),
        modified,
        identity,
        readonly: metadata.permissions().readonly(),
        mode,
    })
}

impl WritableTarget {
    pub(crate) fn review_stamp(&self) -> io::Result<Stamp> {
        let file = self.open_regular_nofollow()?;
        let metadata = file.metadata()?;
        let text = super::super::read_utf8_limited(file, FILE_BYTES)?;
        stamp(&text, &metadata)
    }

    fn matches_entry(&self, entry: &Entry) -> io::Result<()> {
        let current = self.review_stamp()?;
        if entry.stamp.as_ref() != Some(&current) || current.hash != digest(entry.after.as_bytes())
        {
            return Err(io_error(
                "conflict: file identity, metadata or contents changed",
            ));
        }
        Ok(())
    }

    // Never replace the current path during recovery. Detach, verify again,
    // then link-publish into an absent name. Retain detached bytes for races/crashes.
    fn recover(&self, entry: &Entry, held: &Dir, review: &FileReview) -> io::Result<()> {
        let _guard = WriteGuard::acquire(&self.parent)?;
        self.matches_entry(entry)?;
        self.parent.rename(&self.file_name, held, "after")?;
        sync_directory(held)?;
        sync_directory(&self.parent)?;
        #[cfg(test)]
        if review
            .takeover_after_detach
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.parent
                .write(&self.file_name, b"user takeover during recovery")?;
        }
        #[cfg(not(test))]
        let _ = review;
        let detached = WritableTarget {
            parent: held.try_clone()?,
            file_name: "after".into(),
            capture: None,
        };
        let result = detached.matches_entry(entry).and_then(|()| {
            if let Some(before) = &entry.before {
                self.publish_before(before, entry, held)?;
            }
            sync_directory(&self.parent)?;
            Ok(())
        });
        if result.is_err() {
            // Best effort, no-clobber compensation. If a new occupant exists,
            // retain after in the reported recovery directory, never replace it.
            let _ = held.hard_link("after", &self.parent, &self.file_name);
        }
        result
    }

    fn publish_before(&self, content: &str, entry: &Entry, held: &Dir) -> io::Result<()> {
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut temp = held.open_with("before", &options)?;
        temp.write_all(content.as_bytes())?;
        let mut permissions = temp.metadata()?.permissions();
        permissions.set_readonly(entry.before_readonly);
        #[cfg(unix)]
        {
            use cap_std::fs::PermissionsExt;
            if let Some(mode) = entry.before_mode {
                permissions = cap_std::fs::Permissions::from_mode(mode);
            }
        }
        temp.set_permissions(permissions)?;
        temp.sync_all()?;
        drop(temp);
        sync_directory(held)?;
        held.hard_link("before", &self.parent, &self.file_name)
    }
}

impl FileReview {
    pub fn preview(&self, project: &Project, ledger: &Ledger) -> Vec<(String, String)> {
        ledger
            .files
            .iter()
            .map(|e| {
                let result = eligible(e).and_then(|()| target(project, &e.path)?.matches_entry(e));
                (
                    e.path.clone(),
                    match result {
                        Ok(()) => "safe".into(),
                        Err(err) => err.to_string(),
                    },
                )
            })
            .collect()
    }

    pub fn apply(&self, project: &Project, mut ledger: Ledger) -> io::Result<Ledger> {
        if self.active.lock().map_err(io_error)?.is_some() {
            return Err(io_error("run is active"));
        }
        let current = self
            .load(&ledger.session, ledger.turn)?
            .ok_or_else(|| io_error("recovery record missing"))?;
        if serde_json::to_vec(&current).map_err(io_error)?
            != serde_json::to_vec(&ledger).map_err(io_error)?
        {
            return Err(io_error("recovery state changed; preview again"));
        }
        for index in 0..ledger.files.len() {
            if ledger.files[index].status == "restored" {
                continue;
            }
            if let Err(error) = self.restore_one(project, &mut ledger, index) {
                ledger.files[index].note = error.to_string();
                self.save(&ledger)?;
                break;
            }
        }
        Ok(ledger)
    }

    fn restore_one(&self, project: &Project, ledger: &mut Ledger, index: usize) -> io::Result<()> {
        #[cfg(test)]
        if self
            .fail_restore_index
            .load(std::sync::atomic::Ordering::SeqCst)
            == index
        {
            return Err(io_error("injected recovery failure"));
        }
        let entry = &ledger.files[index];
        eligible(entry)?;
        let target = target(project, &entry.path)?;
        target.matches_entry(entry)?;
        let name = format!(".clat-restore-{}", uuid::Uuid::new_v4());
        target.parent.create_dir(&name)?;
        let held = store::child(&target.parent, &name)?;
        sync_directory(&target.parent)?;
        let relative = Path::new(&entry.path)
            .parent()
            .unwrap_or(Path::new(""))
            .join(&name);
        ledger.files[index].recovery_path = Some(relative.to_string_lossy().into_owned());
        ledger.files[index].status = "restoring".into();
        self.save(ledger)?;
        target.recover(&ledger.files[index], &held, self)?;
        ledger.files[index].status = "restored".into();
        ledger.files[index].note.clear();
        self.save(ledger)
    }
}

fn eligible(entry: &Entry) -> io::Result<()> {
    if entry.mixed {
        return Err(io_error(
            "mixed: another writer intervened between captured operations",
        ));
    }
    if entry.status != "captured" {
        return Err(io_error(format!(
            "{}: inspect retained recovery material",
            entry.status
        )));
    }
    Ok(())
}

fn sync_directory(dir: &Dir) -> io::Result<()> {
    #[cfg(unix)]
    dir.open(".")?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn target(project: &Project, path: &str) -> io::Result<WritableTarget> {
    super::super::validate_relative_path(Path::new(path))?;
    project.strict_file_target(Path::new(path))
}
