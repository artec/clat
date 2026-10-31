//! Host lifetime is shared; the execution service exists only after plugin mount.
use super::*;
pub(crate) struct HostScheduler {
    control: Arc<crate::control_storage::ControlStorage>,
    service: std::sync::OnceLock<Arc<SchedulerService>>,
}
impl HostScheduler {
    pub(crate) fn new(control: Arc<crate::control_storage::ControlStorage>) -> Self {
        Self {
            control,
            service: Default::default(),
        }
    }
    pub(crate) fn mount(&self) -> Arc<SchedulerService> {
        self.service
            .get_or_init(|| Arc::new(SchedulerService::new(self.control.clone())))
            .clone()
    }
    #[cfg(test)]
    pub(crate) fn initialized(&self) -> bool {
        self.service.get().is_some()
    }
}
