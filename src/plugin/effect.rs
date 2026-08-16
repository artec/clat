use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisposeError {
    message: String,
}

impl DisposeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for DisposeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DisposeError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisposeErrors {
    errors: Vec<DisposeError>,
}

impl DisposeErrors {
    pub fn new(errors: Vec<DisposeError>) -> Self {
        Self { errors }
    }

    pub fn into_errors(self) -> Vec<DisposeError> {
        self.errors
    }
}

impl fmt::Display for DisposeErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} cleanup error(s)", self.errors.len())?;
        for error in &self.errors {
            write!(formatter, "; {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DisposeErrors {}

type Disposer = Box<dyn FnOnce() -> Result<(), DisposeError> + Send + 'static>;

#[derive(Default)]
pub struct EffectScope {
    disposers: Vec<Disposer>,
    closed: bool,
}

impl EffectScope {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn defer(&mut self, disposer: impl FnOnce() -> Result<(), DisposeError> + Send + 'static) {
        assert!(!self.closed, "cannot register an effect after scope close");
        self.disposers.push(Box::new(disposer));
    }

    /// Registers ownership in the same operation that exposes a resource.
    /// The disposer receives its own `Arc`, while the caller receives another
    /// clone for service registration or use during mount.
    pub fn acquire<T>(
        &mut self,
        resource: T,
        dispose: impl FnOnce(Arc<T>) -> Result<(), DisposeError> + Send + 'static,
    ) -> Arc<T>
    where
        T: Send + Sync + 'static,
    {
        let resource = Arc::new(resource);
        let owned = Arc::clone(&resource);
        self.defer(move || dispose(owned));
        resource
    }

    /// Closes once, in reverse registration order. Every disposer is attempted
    /// even when an earlier one fails or panics; panics become cleanup errors.
    pub fn close(&mut self) -> Result<(), DisposeErrors> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let mut errors = Vec::new();
        while let Some(disposer) = self.disposers.pop() {
            match catch_unwind(AssertUnwindSafe(disposer)) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(error),
                Err(payload) => errors.push(DisposeError::new(format!(
                    "cleanup panicked: {}",
                    panic_message(payload)
                ))),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(DisposeErrors::new(errors))
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".into()
    }
}
