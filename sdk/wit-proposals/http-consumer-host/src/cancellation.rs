//! Cancellation observation supplied by the embedding runtime.
use std::sync::Arc;
#[derive(Clone)]
pub struct CancelToken(Arc<dyn Fn() -> bool + Send + Sync>);
impl CancelToken {
    pub fn from_check(check: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(check))
    }
    pub fn is_cancelled(&self) -> bool {
        (self.0)()
    }
}
