//! User/project Skill management. Settings is a Host control plane, not a plugin store.
use super::{ApplicationError, TrustedProjectApplication};
use crate::skills::SkillFiles;
use serde_json::{Value, json};
use std::path::Path;
#[cfg(test)]
mod tests;

impl TrustedProjectApplication {
    fn managed_skill_root(&self, scope: &str) -> Result<(&Path, bool), ApplicationError> {
        match scope {
            "user" => Ok((self.host_storage.root(), true)),
            "project" => Ok((self.project.root(), false)),
            _ => Err(ApplicationError::new("Skill scope must be user or project")),
        }
    }

    pub fn skill_manage_list(&self) -> Result<Value, ApplicationError> {
        let user =
            SkillFiles::names(self.host_storage.root(), true).map_err(ApplicationError::new)?;
        let project =
            SkillFiles::names(self.project.root(), false).map_err(ApplicationError::new)?;
        let effective = self.skills_overview()?;
        Ok(json!({
            "user":user,"project":project,"effective":effective,
            "project_root":self.project.root(),
            "builtin_enabled":self.builtin_available("builtin.skills")
        }))
    }

    pub fn skill_manage_get(&self, scope: &str, name: &str) -> Result<Value, ApplicationError> {
        let (root, user) = self.managed_skill_root(scope)?;
        SkillFiles::read(root, user, name).map_err(ApplicationError::new)
    }

    pub fn skill_manage_save(
        &self,
        scope: &str,
        name: &str,
        revision: &str,
        text: &str,
    ) -> Result<Value, ApplicationError> {
        let (root, user) = self.managed_skill_root(scope)?;
        let _guard = self.host_storage.skill_writes.lock().expect("Skill edits");
        SkillFiles::save(root, user, name, revision, text).map_err(ApplicationError::new)
    }

    pub fn skill_manage_remove(
        &self,
        scope: &str,
        name: &str,
        revision: &str,
    ) -> Result<Value, ApplicationError> {
        let (root, user) = self.managed_skill_root(scope)?;
        let _guard = self.host_storage.skill_writes.lock().expect("Skill edits");
        SkillFiles::remove(root, user, name, revision).map_err(ApplicationError::new)
    }
}
