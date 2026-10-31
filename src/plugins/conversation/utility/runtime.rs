//! Actual mounted auxiliary execution ownership; prepared handles retain no lease after close.
use super::{UtilityFailure, UtilityModel, UtilityOutput, UtilityTask};
use crate::plugin::lifecycle::{Lifecycle, Producer};
use std::sync::Arc;

pub(crate) struct UtilityRuntime {
    model: Arc<dyn UtilityModel>,
    lifetime: Arc<Lifecycle>,
}
impl UtilityRuntime {
    pub(crate) fn new(model: Arc<dyn UtilityModel>, lease: Option<Arc<dyn Send + Sync>>) -> Self {
        #[cfg(test)]
        crate::test_support::execution_observation::record("utility.create");
        let lifetime = Arc::new(Lifecycle::default());
        if let Some(lease) = lease {
            lifetime.attach(lease);
        }
        Self { model, lifetime }
    }
    pub(crate) fn generate(
        &self,
        task: UtilityTask,
        config: &crate::ModelConfig,
        credentials: &crate::ProviderCredentials,
        conversation: &str,
        cancel: &crate::CancelToken,
    ) -> Result<UtilityOutput, UtilityFailure> {
        let producer = self
            .producer(cancel)
            .map_err(|_| UtilityFailure::Cancelled)?;
        let result = self
            .model
            .generate(task, config, credentials, conversation, cancel);
        producer
            .commit(|| result)
            .map_err(|_| UtilityFailure::Cancelled)?
    }
    pub(crate) fn enabled(&self, task: UtilityTask) -> bool {
        self.model.enabled(task)
    }
    pub(crate) fn producer(&self, cancel: &crate::CancelToken) -> Result<Producer, String> {
        self.lifetime.producer_for(cancel)
    }
    pub(crate) fn revoke(&self) {
        self.lifetime.close();
    }
    pub(crate) fn active(&self) -> bool {
        self.lifetime.active()
    }
    pub(crate) fn close(&self) -> Result<(), String> {
        self.revoke();
        if self.active() || self.lifetime.failed() {
            Err("cleanup_failed".into())
        } else {
            Ok(())
        }
    }
    #[cfg(test)]
    pub(crate) fn with_model(&self, model: Arc<dyn UtilityModel>) -> Self {
        Self {
            model,
            lifetime: self.lifetime.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counter(Arc<AtomicUsize>);
    impl UtilityModel for Counter {
        fn enabled(&self, _: UtilityTask) -> bool {
            true
        }
        fn generate(
            &self,
            _: UtilityTask,
            _: &crate::ModelConfig,
            _: &crate::ProviderCredentials,
            _: &str,
            _: &crate::CancelToken,
        ) -> Result<UtilityOutput, UtilityFailure> {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("closed runtime must not reach model");
        }
    }
    #[test]
    fn closed_auxiliary_runtime_rejects_direct_model_dispatch() {
        let count = Arc::new(AtomicUsize::new(0));
        let runtime = UtilityRuntime::new(Arc::new(Counter(count.clone())), None);
        runtime.close().unwrap();
        assert!(matches!(
            runtime.generate(
                UtilityTask::SessionTitle,
                &crate::ModelConfig::default(),
                &crate::ProviderCredentials::for_protocol(crate::ModelConfig::default().protocol),
                "old context",
                &crate::CancelToken::new()
            ),
            Err(UtilityFailure::Cancelled)
        ));
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
