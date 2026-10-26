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

impl Project {
    /// Bounded immediate children; every traversed component rejects symlinks.
    pub(crate) fn directory_snapshot(&self, relative: &str) -> io::Result<Vec<(String, bool)>> {
        let mut dir = Dir::open_ambient_dir(self.root.canonicalize()?, ambient_authority())?;
        if !relative.is_empty() {
            let path = Path::new(relative);
            validate_relative_path(path)?;
            for part in path.components() {
                dir = crate::session::root_dir::SessionRootDir::open_child(
                    &dir,
                    Path::new(part.as_os_str()),
                )?;
            }
        }
        let mut entries = Vec::new();
        for entry in dir.entries()?.take(10001) {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_symlink() || !ty.is_file() && !ty.is_dir() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                entries.push((name.to_owned(), ty.is_dir()));
            }
        }
        Ok(entries)
    }
}
