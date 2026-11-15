//! Read-only task discovery across the storage root's trusted projects.
use super::*;
use crate::{
    background_jobs::{JobCapacity, JobView, read_unmounted_jobs},
    scheduler::{Delivery, Rule, SchedulerService},
    session::{key::ProjectKey, use_cases::query::QueryReader},
};
use std::{collections::BTreeMap, path::Path};

pub(crate) struct TaskDirectoryReader {
    control: Arc<ControlStorage>,
    projects: Vec<ProjectKey>,
    titles: BTreeMap<String, String>,
    live_jobs: BTreeMap<String, JobsReader>,
    cold_jobs: Option<(QueryReader, Arc<JobCapacity>)>,
    scheduler: Option<Arc<SchedulerService>>,
}

#[derive(Default, serde::Serialize)]
pub(crate) struct TaskDirectory {
    pub projects: Vec<TaskProject>,
    pub errors: Vec<crate::i18n::UiMessage>,
}

#[derive(serde::Serialize)]
pub(crate) struct TaskProject {
    root: String,
    title: String,
    jobs: Vec<JobView>,
    rules: Vec<Rule>,
    deliveries: Vec<Delivery>,
}

impl HostApplication {
    pub(crate) fn task_directory_reader(&self) -> TaskDirectoryReader {
        let control = self.storage.control.clone();
        let projects = control.trusted_project_keys();
        let titles = self
            .workspace_directory()
            .into_iter()
            .map(|row| (row.root, row.title))
            .collect();
        let jobs_enabled = self.startup_availability["builtin.jobs"].enabled;
        let mut live_jobs = BTreeMap::new();
        let mut cold_jobs = None;
        let mut scheduler = None;
        for app in self.projects.values() {
            let app = app.lock().expect("application lock");
            if jobs_enabled {
                live_jobs.insert(app.schedule_project(), app.jobs_reader());
                cold_jobs.get_or_insert_with(|| {
                    (
                        app.sessions.query_reader(),
                        self.storage.job_capacity.clone(),
                    )
                });
            }
            if scheduler.is_none() {
                scheduler = app.scheduler.clone();
            }
        }
        TaskDirectoryReader {
            control,
            projects,
            titles,
            live_jobs,
            cold_jobs,
            scheduler,
        }
    }
}

impl TaskDirectoryReader {
    pub(crate) fn read(&self) -> Result<TaskDirectory, ApplicationError> {
        self.check_scope()?;
        let mut result = TaskDirectory::default();
        let (rules, deliveries) = match self
            .scheduler
            .as_ref()
            .map(|port| port.task_directory())
            .transpose()
        {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => {
                result
                    .errors
                    .push(super::schedules::schedule_error(error).presentation());
                (Vec::new(), Vec::new())
            }
        };
        let mut by_project = BTreeMap::new();
        for key in &self.projects {
            let Some(root) = &key.header_cwd else {
                continue;
            };
            let title = self.titles.get(root).cloned().unwrap_or_else(|| {
                Path::new(root)
                    .file_name()
                    .map_or_else(|| root.clone(), |name| name.to_string_lossy().into_owned())
            });
            let jobs = self.jobs(key, root).unwrap_or_else(|error| {
                result.errors.push(error.presentation());
                Vec::new()
            });
            by_project.insert(
                root.clone(),
                TaskProject {
                    root: root.clone(),
                    title,
                    jobs,
                    rules: Vec::new(),
                    deliveries: Vec::new(),
                },
            );
        }
        for rule in rules {
            if let Some(row) = by_project.get_mut(&rule.scope.project) {
                row.rules.push(rule);
            }
        }
        for delivery in deliveries {
            if let Some(row) = by_project.get_mut(&delivery.scope.project) {
                row.deliveries.push(delivery);
            }
        }
        self.check_scope()?;
        result.projects = by_project.into_values().collect();
        Ok(result)
    }

    fn check_scope(&self) -> Result<(), ApplicationError> {
        if self.control.trusted_project_keys() == self.projects {
            return Ok(());
        }
        Err(ApplicationError::presented(crate::i18n::UiMessage::new(
            "pages.task_scope_changed",
        )))
    }

    fn jobs(&self, key: &ProjectKey, root: &str) -> Result<Vec<JobView>, ApplicationError> {
        if let Some(reader) = self.live_jobs.get(root) {
            return reader.list();
        }
        match &self.cold_jobs {
            Some((reader, capacity)) => read_unmounted_jobs(reader, key, capacity)
                .map_err(|error| ApplicationError::presented(error.presentation())),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(all(test, feature = "runtime-tests"))]
mod tests {
    use super::*;

    #[test]
    fn task_directory_reads_unmounted_trusted_projects_without_recovery_or_selection() {
        let (root, mut host, source, current) = super::super::host_tests::fixture("task-directory");
        let other = host.attach(source.clone()).unwrap();
        let (rule_id, job_id, key) = {
            let mut app = other.lock().unwrap();
            if let Some(worker) = &mut app.title_worker {
                worker.shutdown().unwrap();
            }
            crate::test_support::configure_test_model(&app);
            super::super::tests::run(&mut app, "seed task directory").unwrap();
            let session = app.current_session_id().unwrap();
            let draft = super::super::tests::schedule_draft(&session, 1000);
            let rule = app.schedule_create(draft).unwrap();
            let job = app
                .require_jobs()
                .unwrap()
                .seed_registered_for_test(&session);
            (
                rule.id,
                job,
                crate::session::key::SessionKey {
                    project: ProjectKey::from_cwd(&app.schedule_project()),
                    id: session,
                },
            )
        };
        drop(other);
        host.close().unwrap();
        drop(host);

        let app = BootstrapApplication::open(current.clone(), root.join("state"))
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(crate::test_support::TestProviderPlugin {
                behavior: crate::test_support::TestBehavior::Success,
            }))
            .unwrap();
        let mut host = HostApplication::new(app);
        let local = host.attach(current).unwrap();
        let session_reader = local.lock().unwrap().sessions.query_reader();
        let before = session_reader.revision(&key).unwrap();
        let projects = host.project_roots();
        let directory = host.task_directory_reader().read().unwrap();
        assert!(directory.errors.is_empty(), "{:?}", directory.errors);
        let listed = directory
            .projects
            .iter()
            .find(|project| Some(&project.root) == key.project.header_cwd.as_ref())
            .unwrap();
        assert!(listed.rules.iter().any(|rule| rule.id == rule_id));
        assert!(
            listed
                .jobs
                .iter()
                .any(|job| job.id == job_id && job.state == "interrupted")
        );
        assert_eq!(host.project_roots(), projects);
        assert_eq!(session_reader.revision(&key).unwrap(), before);
        assert!(local.lock().unwrap().current_session_id().is_none());

        let stale = host.task_directory_reader();
        host.storage.control.remove_trust(source.root()).unwrap();
        assert_eq!(
            stale.read().err().unwrap().presentation().key,
            "pages.task_scope_changed"
        );
        assert!(
            host.task_directory_reader()
                .read()
                .unwrap()
                .projects
                .iter()
                .all(|project| Some(&project.root) != key.project.header_cwd.as_ref())
        );
        drop(local);
        host.close().unwrap();
        drop(host);
        crate::test_support::cleanup_tree(&root);
    }
}
