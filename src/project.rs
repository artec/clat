use std::env;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    root: PathBuf,
}

impl Project {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn current() -> io::Result<Self> {
        Ok(Self::new(env::current_dir()?))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_existing(&self, relative: impl AsRef<Path>) -> io::Result<PathBuf> {
        let relative = relative.as_ref();
        validate_relative_path(relative)?;

        let root = self.root.canonicalize()?;
        let candidate = root.join(relative).canonicalize()?;

        if candidate.starts_with(&root) {
            Ok(candidate)
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "path resolves outside the project root",
            ))
        }
    }

    pub fn relative_path(&self, path: &Path) -> io::Result<PathBuf> {
        let root = self.root.canonicalize()?;
        let path = path.canonicalize()?;
        path.strip_prefix(root)
            .map(Path::to_path_buf)
            .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "path is outside project"))
    }
}

fn validate_relative_path(path: &Path) -> io::Result<()> {
    if path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "absolute paths are not allowed",
        ));
    }

    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "parent traversal is not allowed",
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = env::temp_dir().join(format!("clat-project-test-{unique}"));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn resolves_paths_inside_project() {
        let root = temp_dir();
        fs::write(root.join("README.md"), "hello").expect("file");
        let project = Project::new(&root);

        let resolved = project.resolve_existing("README.md").expect("resolve");
        assert!(resolved.starts_with(root.canonicalize().expect("root")));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = temp_dir();
        let project = Project::new(&root);

        let error = project
            .resolve_existing("../secret")
            .expect_err("must reject");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        fs::remove_dir_all(root).expect("cleanup");
    }
}
