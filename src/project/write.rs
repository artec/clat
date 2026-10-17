use super::*;

impl WritableTarget {
    pub(super) fn prepare_capture(&self, content: &str) -> io::Result<Option<(String, u64)>> {
        let Some((review, path)) = &self.capture else {
            return Ok(None);
        };
        if content.len() > review::FILE_BYTES {
            return Err(review::io_error("native recovery file limit reached"));
        }
        let (before, stamp) = match self.open_regular_nofollow() {
            Ok(mut file) => {
                let metadata = file.metadata()?;
                let before = read_utf8_limited(&mut file, review::FILE_BYTES)?;
                let after_read = file.metadata()?;
                if metadata.modified()? != after_read.modified()?
                    || metadata.len() != after_read.len()
                {
                    return Err(review::io_error(
                        "file changed while capturing baseline; retry",
                    ));
                }
                let stamp = review::restore::stamp(&before, &metadata)?;
                (Some(before), Some(stamp))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (None, None),
            Err(e) => return Err(e),
        };
        review.prepare(path, before, content, stamp)
    }

    // A create-only attempt must not read an external file that appeared
    // after the absence check. Failed publication remains a prepared record,
    // following the existing recovery protocol for uncertain commits.
    pub(super) fn prepare_create_capture(
        &self,
        content: &str,
    ) -> io::Result<Option<(String, u64)>> {
        let Some((review, path)) = &self.capture else {
            return Ok(None);
        };
        if content.len() > review::FILE_BYTES {
            return Err(review::io_error("native recovery file limit reached"));
        }
        review.prepare(path, None, content, None)
    }

    pub(super) fn finish_capture(
        &self,
        owner: Option<(String, u64)>,
        stamp: review::Stamp,
    ) -> io::Result<()> {
        if let (Some((review, path)), Some(owner)) = (&self.capture, owner) {
            review.finish(&owner, path, stamp)?;
        }
        Ok(())
    }

    pub(super) fn publish(
        &self,
        content: &str,
        metadata: Option<cap_std::fs::Metadata>,
    ) -> io::Result<review::Stamp> {
        self.publish_mode(content, metadata, true, false)
    }

    pub(super) fn publish_create(&self, content: &str) -> io::Result<review::Stamp> {
        self.publish_create_mode(content, false)
    }

    pub(super) fn publish_create_mode(
        &self,
        content: &str,
        private: bool,
    ) -> io::Result<review::Stamp> {
        self.publish_mode(content, None, false, private)
    }

    fn publish_mode(
        &self,
        content: &str,
        metadata: Option<cap_std::fs::Metadata>,
        replace: bool,
        private: bool,
    ) -> io::Result<review::Stamp> {
        let temp_name = self.create_temp_file_name();
        let result = (|| -> io::Result<review::Stamp> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            if private {
                use cap_std::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            #[cfg(not(unix))]
            let _ = private;
            let mut temp = self.parent.open_with(&temp_name, &options)?;
            temp.write_all(content.as_bytes())?;
            if let Some(metadata) = metadata
                && metadata.is_file()
            {
                temp.set_permissions(metadata.permissions())?;
            }
            temp.sync_all()?;
            let stamp = review::restore::stamp(content, &temp.metadata()?)?;
            drop(temp);
            self.reject_final_symlink()?;
            if replace {
                self.parent
                    .rename(&temp_name, &self.parent, &self.file_name)?;
            } else {
                // Linking a complete same-directory temporary file publishes it
                // atomically and refuses an existing destination. Unsupported
                // filesystems fail closed; never fall back to replacing rename.
                self.parent
                    .hard_link(&temp_name, &self.parent, &self.file_name)?;
            }
            Ok(stamp)
        })();
        if result.is_err() || !replace {
            let _ = self.parent.remove_file(&temp_name);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_guard_publish_refuses_an_external_entry_after_target_resolution() {
        let root = std::env::temp_dir().join(format!("clat-create-race-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let project = Project::new(&root);
        let target = project
            .writable_target("late", true, crate::permission::WriteScope::ProjectRoot)
            .unwrap();
        // Simulate an external writer after the absence check. The publisher
        // itself must refuse the collision, independently of advisory locks.
        std::fs::write(root.join("late"), "external owner").unwrap();
        let error = target.publish_create("agent fragment").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(root.join("late")).unwrap(),
            "external owner"
        );
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "failed publication removes its temporary file"
        );
        crate::test_support::cleanup_tree(&root);
    }
}
