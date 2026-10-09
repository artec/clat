use super::*;

impl Project {
    pub(crate) fn writable_roots(
        &self,
    ) -> io::Result<std::sync::Arc<crate::sandbox::roots::WritableRoots>> {
        self.roots
            .read()
            .map_err(|_| io::Error::other("scratch: roots lock poisoned"))?
            .clone()
            .ok_or_else(|| io::Error::other("scratch: no active run"))
    }

    pub(super) fn rooted_absolute_target(
        &self,
        requested: &Path,
        create_parents: bool,
    ) -> io::Result<WritableTarget> {
        let roots = self.writable_roots()?;
        // Resolve the existing ancestor before mkdir, then traverse the
        // remaining components using the selected root capability.
        let resolved = resolve_absolute_candidate(requested)?;
        let (index, root) = roots
            .paths()
            .iter()
            .enumerate()
            .filter(|(_, root)| resolved.starts_with(root))
            .max_by_key(|(_, root)| root.components().count())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "path outside writable roots",
                )
            })?;
        let relative = resolved.strip_prefix(root).map_err(io::Error::other)?;
        validate_relative_path(relative)?;
        let file_name = relative
            .file_name()
            .ok_or_else(|| io::Error::other("path has no file name"))?;
        let root_dir = roots.directory(index)?;
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        if create_parents && !parent.as_os_str().is_empty() {
            root_dir.create_dir_all(parent)?;
        }
        let parent_dir = if parent.as_os_str().is_empty() {
            root_dir
        } else {
            root_dir.open_dir(parent)?
        };
        let mut target = WritableTarget {
            parent: parent_dir,
            file_name: file_name.to_owned(),
            capture: None,
        };
        if root == &self.root.canonicalize()? {
            target.capture = self
                .review
                .clone()
                .map(|review| (review, relative.to_string_lossy().into_owned()));
        }
        Ok(target)
    }
}

fn resolve_absolute_candidate(requested: &Path) -> io::Result<PathBuf> {
    let parent = requested
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    for ancestor in parent.ancestors() {
        match ancestor.canonicalize() {
            Ok(canonical) => {
                let suffix = requested.strip_prefix(ancestor).map_err(io::Error::other)?;
                validate_relative_path(suffix)?;
                return Ok(canonical.join(suffix));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("no existing ancestor for absolute target"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn scratch_symlink_escape_is_refused_before_parent_creation() {
        let (_, workspace) = crate::test_support::roots("scratch-fence");
        std::fs::create_dir_all(&workspace).unwrap();
        let project = Project::new(&workspace);
        let roots = crate::sandbox::roots::WritableRoots::create(&workspace).unwrap();
        *project.roots.write().unwrap() = Some(roots.clone());
        let victim = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&victim).unwrap();
        std::os::unix::fs::symlink(&victim, roots.scratch().join("escape")).unwrap();
        assert!(
            project
                .writable_target(
                    roots.scratch().join("escape/new/file"),
                    true,
                    crate::permission::WriteScope::WorkspaceRoots
                )
                .is_err()
        );
        assert!(!victim.join("new").exists());
        roots.close().unwrap();
        crate::test_support::cleanup_tree(&workspace);
        crate::test_support::cleanup_tree(&victim);
    }
}
