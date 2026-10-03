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
        let temp_name = self.create_temp_file_name();
        let result = (|| -> io::Result<review::Stamp> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
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
            self.parent
                .rename(&temp_name, &self.parent, &self.file_name)?;
            Ok(stamp)
        })();
        if result.is_err() {
            let _ = self.parent.remove_file(&temp_name);
        }
        result
    }
}
