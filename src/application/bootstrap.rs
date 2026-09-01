use crate::Project;
use crate::control_storage::sentinel;
#[cfg(test)]
use crate::plugin::Plugin;
use std::path::PathBuf;

use super::*;

/// One-shot, in-memory user consent to trust the boot project. It never
/// persists by itself — `authorize_and_mount` commits trust only after the
/// full session-root preflight passed (plan §3.2).
pub struct ProjectAuthorization {
    _private: (),
}

impl ProjectAuthorization {
    pub fn grant() -> Self {
        Self { _private: () }
    }
}

/// Pre-trust state: a zero-write control-plane preflight. No plugin scope,
/// no writable SQLite, no session-root discovery (plan §14.1).
pub struct BootstrapApplication {
    project: Project,
    storage_root: PathBuf,
    /// 交互前端（TUI）置位：挂载后权限策略读共享档位 cell（权限三
    /// 档）。headless exec 不置位——委托保持 SafeByDefault 逐次询问，
    /// 行为零变化（P7）。
    permission_modes: bool,
}

impl BootstrapApplication {
    pub fn open_default(project: Project) -> Result<Self, ApplicationError> {
        let root = sentinel::default_storage_root().map_err(ApplicationError::new)?;
        Self::open(project, root)
    }

    pub fn open(project: Project, storage_root: PathBuf) -> Result<Self, ApplicationError> {
        if std::fs::symlink_metadata(&storage_root).is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(ApplicationError::new(format!(
                "storage root must not be a symbolic link: {}",
                storage_root.display()
            )));
        }
        match sentinel::classify(&storage_root) {
            sentinel::ControlPlaneStatus::Unsupported(reason)
            | sentinel::ControlPlaneStatus::Inconsistent(reason) => {
                return Err(ApplicationError::new(reason));
            }
            _ => {}
        }
        Ok(Self {
            project,
            storage_root,
            permission_modes: false,
        })
    }

    /// Builder：启用权限三档（交互前端专用，见结构体字段说明）。
    pub fn with_permission_modes(mut self) -> Self {
        self.permission_modes = true;
        self
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub(crate) fn storage_root(&self) -> &std::path::Path {
        &self.storage_root
    }

    /// Read-only trust check through the sentinel path — no writable
    /// file is ever created here.
    pub fn is_trusted(&self) -> Result<bool, ApplicationError> {
        match sentinel::classify(&self.storage_root) {
            sentinel::ControlPlaneStatus::Fresh => Ok(false),
            status if status.is_ready() => {
                sentinel::is_trusted_read_only(&self.storage_root, self.project.root())
                    .map_err(ApplicationError::new)
            }
            // LegacySQLite / LegacyConfigOnly: 新世界信任库尚未诞生，
            // 一律未信任（authorize_and_mount 走升级 + add_trust）。
            _ => Ok(false),
        }
    }

    pub fn into_trusted(self) -> Result<TrustedProjectApplication, ApplicationError> {
        if !self.is_trusted()? {
            return Err(ApplicationError::new("project is not trusted"));
        }
        TrustedProjectApplication::mount(
            self.project,
            self.storage_root,
            false,
            self.permission_modes,
        )
    }

    /// Trust + mount in one shot (the only path that persists new trust):
    /// lease → session-root preflight → control commit → Trusted Project.
    pub fn authorize_and_mount(
        self,
        authorization: ProjectAuthorization,
    ) -> Result<TrustedProjectApplication, ApplicationError> {
        let _ = authorization;
        TrustedProjectApplication::mount(
            self.project,
            self.storage_root,
            true,
            self.permission_modes,
        )
    }

    #[cfg(test)]
    pub(crate) fn into_trusted_with_provider(
        self,
        provider: Arc<dyn Plugin>,
    ) -> Result<TrustedProjectApplication, ApplicationError> {
        if !self.is_trusted()? {
            return Err(ApplicationError::new("project is not trusted"));
        }
        TrustedProjectApplication::mount_with_providers(
            self.project,
            self.storage_root,
            false,
            Some(vec![provider]),
            self.permission_modes,
        )
    }

    #[cfg(test)]
    pub(crate) fn authorize_and_mount_with_provider(
        self,
        provider: Arc<dyn Plugin>,
    ) -> Result<TrustedProjectApplication, ApplicationError> {
        TrustedProjectApplication::mount_with_providers(
            self.project,
            self.storage_root,
            true,
            Some(vec![provider]),
            self.permission_modes,
        )
    }
}
