use super::*;

impl Project {
    /// Browser reads reject every symlink component, even links inside the project.
    pub(crate) fn file_snapshot(
        &self,
        relative: &str,
        cap: usize,
    ) -> io::Result<(Vec<u8>, cap_std::fs::Metadata)> {
        let path = Path::new(relative);
        let target = self.strict_file_target(path)?;
        let mut file = target.open_regular_nofollow()?;
        let metadata = file.metadata()?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(cap as u64)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        if metadata.modified()? != after.modified()? || metadata.len() != after.len() {
            return Err(io::Error::other("file changed during preview; retry"));
        }
        Ok((bytes, metadata))
    }

    pub(super) fn strict_file_target(&self, path: &Path) -> io::Result<WritableTarget> {
        validate_relative_path(path)?;
        let mut parent = Dir::open_ambient_dir(self.root.canonicalize()?, ambient_authority())?;
        if let Some(parts) = path.parent() {
            for part in parts.components() {
                parent = crate::session::root_dir::SessionRootDir::open_child(
                    &parent,
                    Path::new(part.as_os_str()),
                )?;
            }
        }
        Ok(WritableTarget {
            parent,
            file_name: path
                .file_name()
                .ok_or_else(|| io::Error::other("missing file name"))?
                .to_owned(),
            capture: None,
        })
    }
}
