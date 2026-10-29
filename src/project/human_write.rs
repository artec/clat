//! Byte-preserving human recovery writes share the same parent lock as Agent writes.
use super::*;
impl Project {
    pub(crate) fn replace_human_file(
        &self,
        path: &str,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> io::Result<()> {
        Project::new(self.root())
            .writable_target(path, false, crate::WriteScope::ProjectRoot)?
            .human_replace(expected, next)
    }
}
impl WritableTarget {
    fn human_replace(&self, expected: Option<&[u8]>, next: Option<&[u8]>) -> io::Result<()> {
        let _guard = WriteGuard::acquire(&self.parent)?;
        self.reject_final_symlink()?;
        let metadata = match self.open_regular_nofollow() {
            Ok(file) => {
                let meta = file.metadata()?;
                let Some(expected) = expected else {
                    return Err(io::Error::new(
                        io::ErrorKind::ResourceBusy,
                        "destination exists",
                    ));
                };
                if !reader_matches_expected(file, expected)? {
                    return Err(io::Error::new(io::ErrorKind::ResourceBusy, "file changed"));
                }
                Some(meta)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && expected.is_none() => None,
            Err(error) => return Err(error),
        };
        let Some(bytes) = next else {
            self.parent.remove_file(&self.file_name)?;
            return Ok(());
        };
        let name = self.create_temp_file_name();
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            let mut file = self.parent.open_with(&name, &options)?;
            file.write_all(bytes)?;
            if let Some(meta) = metadata {
                file.set_permissions(meta.permissions())?;
            }
            file.sync_all()?;
            drop(file);
            self.reject_final_symlink()?;
            if expected.is_none() {
                self.parent
                    .hard_link(&name, &self.parent, &self.file_name)?;
                self.parent.remove_file(&name)?;
            } else {
                self.parent.rename(&name, &self.parent, &self.file_name)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.parent.remove_file(&name);
        }
        result
    }
}
