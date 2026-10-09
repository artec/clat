//! The one writable-root authority shared by native tools and OS providers.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub(crate) type RootsSource = Arc<RwLock<Option<Arc<WritableRoots>>>>;

#[derive(Debug)]
pub(crate) struct WritableRoots {
    paths: Vec<PathBuf>,
    scratch: PathBuf,
    handles: RwLock<Vec<cap_std::fs::Dir>>,
}

impl WritableRoots {
    pub(crate) fn create(workspace: &Path) -> io::Result<Arc<Self>> {
        let workspace = workspace.canonicalize()?;
        let temp = std::env::temp_dir().canonicalize()?;
        let scratch = temp.join(format!("clat-run-{}", uuid::Uuid::new_v4()));
        if scratch.starts_with(&workspace) {
            return Err(io::Error::other(
                "scratch: system temp is inside the workspace",
            ));
        }
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            let mut builder = builder;
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
            builder
        };
        builder.create(&scratch)?;
        let mut paths = match Self::base_paths(&workspace) {
            Ok(paths) => paths,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&scratch);
                return Err(error);
            }
        };
        if !paths.contains(&scratch) {
            paths.push(scratch.clone());
        }
        let handles = paths
            .iter()
            .map(|path| {
                if path == &scratch {
                    let parent =
                        cap_std::fs::Dir::open_ambient_dir(&temp, cap_std::ambient_authority())?;
                    cap_primitives::fs::open_dir_nofollow(
                        &parent.into_std_file(),
                        Path::new(scratch.file_name().unwrap()),
                    )
                    .map(cap_std::fs::Dir::from_std_file)
                } else {
                    cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority())
                }
            })
            .collect::<io::Result<Vec<_>>>();
        let handles = match handles {
            Ok(handles) => handles,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&scratch);
                return Err(error);
            }
        };
        Ok(Arc::new(Self {
            paths,
            scratch,
            handles: RwLock::new(handles),
        }))
    }

    pub(crate) fn base_paths(workspace: &Path) -> io::Result<Vec<PathBuf>> {
        let paths = vec![workspace.canonicalize()?];
        #[cfg(unix)]
        let paths = {
            let mut paths = paths;
            for path in [PathBuf::from("/tmp"), std::env::temp_dir()] {
                let path = path.canonicalize()?;
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
            paths
        };
        Ok(paths)
    }

    pub(crate) fn close(&self) -> io::Result<()> {
        let mut handles = self
            .handles
            .write()
            .map_err(|_| io::Error::other("scratch: directory lock poisoned"))?;
        if !self.scratch.try_exists()? {
            handles.clear();
            return Ok(());
        }
        #[cfg(windows)]
        crate::sandbox::windows::revoke_scratch(&self.scratch).map_err(io::Error::other)?;
        // Windows directory capabilities deliberately disallow deletion while
        // open. Release our handles after ACL revocation, before reclaiming.
        handles.clear();
        match std::fs::remove_dir_all(&self.scratch) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    pub(crate) fn directory(&self, index: usize) -> io::Result<cap_std::fs::Dir> {
        self.handles
            .read()
            .map_err(|_| io::Error::other("scratch: directory lock poisoned"))?
            .get(index)
            .ok_or_else(|| io::Error::other("scratch: directory capabilities closed"))?
            .try_clone()
    }

    pub(crate) fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub(crate) fn scratch(&self) -> &Path {
        &self.scratch
    }
}

impl Drop for WritableRoots {
    fn drop(&mut self) {
        // remove_dir_all does not follow directory symlinks, including the root.
        let _ = self.close();
    }
}

impl super::SandboxService {
    pub(crate) fn for_project(
        project: crate::Project,
        mode: super::SandboxModeSource,
    ) -> Result<Self, String> {
        let mut service = Self::new(project.root().to_path_buf(), mode)?;
        service.roots = project.roots.clone();
        Ok(service)
    }

    pub(crate) fn writable_roots(&self) -> Result<Arc<WritableRoots>, String> {
        self.roots
            .read()
            .map_err(|_| "scratch: roots lock poisoned".to_owned())?
            .clone()
            .ok_or_else(|| "scratch: no active run".to_owned())
    }

    pub(crate) fn bind_scratch(&self) -> Result<(), String> {
        let roots = WritableRoots::create(&self.project_root)
            .map_err(|error| format!("scratch: cannot create writable roots: {error}"))?;
        let mut current = self
            .roots
            .write()
            .map_err(|_| "scratch: roots lock poisoned".to_owned())?;
        if current.is_some() {
            return Err("scratch: another run owns writable roots".into());
        }
        *current = Some(roots);
        Ok(())
    }

    pub(crate) fn clear_scratch(&self) -> Result<(), String> {
        if let Some(roots) = self.roots.write().expect("scratch roots lock").take() {
            roots
                .close()
                .map_err(|e| format!("scratch: cleanup failed: {e}"))?;
        }
        Ok(())
    }

    pub(crate) fn roots_source(&self) -> RootsSource {
        self.roots.clone()
    }

    #[cfg(target_os = "macos")]
    pub(super) fn profile_roots(&self) -> Result<Vec<PathBuf>, String> {
        if let Ok(roots) = self.writable_roots() {
            return Ok(roots.paths().to_vec());
        }
        WritableRoots::base_paths(&self.project_root).map_err(|e| format!("sandbox roots: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_is_private_unique_external_and_reclaimed() {
        let workspace = std::env::current_dir().unwrap();
        let first = WritableRoots::create(&workspace).unwrap();
        let second = WritableRoots::create(&workspace).unwrap();
        assert_ne!(first.scratch(), second.scratch());
        assert!(
            !first
                .scratch()
                .starts_with(workspace.canonicalize().unwrap())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(first.scratch())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        #[cfg(windows)]
        assert_eq!(first.paths().len(), 2);
        let path = first.scratch().to_owned();
        std::fs::write(path.join("script.py"), "print(1)").unwrap();
        first.close().unwrap();
        assert!(!path.exists());
        assert!(second.scratch().exists());
    }

    #[test]
    fn close_releases_directory_capabilities_before_reclaiming_scratch() {
        let roots = WritableRoots::create(&std::env::current_dir().unwrap()).unwrap();
        let index = roots
            .paths()
            .iter()
            .position(|path| path == roots.scratch())
            .unwrap();
        let scratch = roots.scratch().to_owned();
        roots
            .directory(index)
            .unwrap()
            .write("marker", "temporary")
            .unwrap();
        roots
            .close()
            .expect("owned capability must not block scratch deletion");
        assert!(!scratch.exists());
        roots.close().expect("close is idempotent");
        assert!(
            roots.directory(index).is_err(),
            "closed roots cannot mint more writable handles"
        );
    }

    #[test]
    fn unavailable_workspace_cannot_fall_back_to_temp() {
        let missing = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        assert!(WritableRoots::create(&missing).is_err());
        assert!(!missing.exists());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod profile_tests {
    use super::*;

    #[test]
    fn seatbelt_and_native_writes_share_the_live_root_authority() {
        let project = crate::Project::new(std::env::current_dir().unwrap());
        let service = super::super::SandboxService::for_project(
            project.clone(),
            super::super::SandboxModeSource::Classic,
        )
        .unwrap();
        service.bind_scratch().unwrap();
        let roots = project.writable_roots().unwrap();
        assert!(Arc::ptr_eq(&roots, &service.writable_roots().unwrap()));
        let profile = super::super::seatbelt_profile(
            super::super::SandboxLevel::WorkspaceWrite,
            &service.profile_roots().unwrap(),
            false,
        )
        .unwrap();
        for root in roots.paths() {
            assert!(profile.contains(&super::super::sbpl_string(root)));
        }
        let read_only = super::super::seatbelt_profile(
            super::super::SandboxLevel::ReadOnly,
            &service.profile_roots().unwrap(),
            false,
        )
        .unwrap();
        assert!(!read_only.contains(&super::super::sbpl_string(roots.scratch())));
        service.clear_scratch().unwrap();
    }
}
