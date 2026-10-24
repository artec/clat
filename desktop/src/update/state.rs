use clat_core::client_ports::presentation::UiMessage;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Checking,
    Downloading,
    Waiting,
    Installing,
}
impl Phase {
    pub fn id(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Checking => "checking",
            Self::Downloading => "downloading",
            Self::Waiting => "waiting",
            Self::Installing => "installing",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::Idle => "update.current",
            Self::Checking => "update.checking",
            Self::Downloading => "update.downloading",
            Self::Waiting => "update.waiting",
            Self::Installing => "update.installing",
        }
    }
}

pub struct State<T> {
    pub auto_check: bool,
    pub status: Option<UiMessage>,
    pub phase: Phase,
    pub pending: Option<T>,
    pub bytes: Option<Arc<Vec<u8>>>,
    pub received: u64,
    pub total: Option<u64>,
    pub limit_hit: bool,
    cancel: Option<watch::Sender<bool>>,
}
impl<T: Clone> State<T> {
    pub fn new(auto_check: bool) -> Self {
        Self {
            auto_check,
            status: None,
            phase: Phase::Idle,
            pending: None,
            bytes: None,
            received: 0,
            total: None,
            limit_hit: false,
            cancel: None,
        }
    }
    pub fn busy(&self) -> bool {
        self.phase != Phase::Idle
    }
    pub fn cancellable(&self) -> bool {
        self.busy() && self.phase != Phase::Installing
    }
    pub fn preserve_page(&self) -> bool {
        matches!(self.phase, Phase::Waiting | Phase::Installing)
    }
    pub fn begin(&mut self, phase: Phase) -> Result<watch::Receiver<bool>, UiMessage> {
        if self.busy() {
            return Err(UiMessage::new("update.busy"));
        }
        if phase == Phase::Downloading && self.pending.is_none() {
            return Err(UiMessage::new("update.check_first"));
        }
        if phase == Phase::Waiting && (self.pending.is_none() || self.bytes.is_none()) {
            return Err(UiMessage::new("update.verify_first"));
        }
        let (tx, rx) = watch::channel(false);
        self.cancel = Some(tx);
        self.phase = phase;
        self.status = Some(UiMessage::new(phase.message()));
        if phase == Phase::Downloading {
            self.received = 0;
            self.total = None;
            self.limit_hit = false;
        }
        Ok(rx)
    }
    pub fn cancel(&self) -> Result<(), UiMessage> {
        if self.phase == Phase::Installing {
            return Err(UiMessage::new("update.cannot_cancel"));
        }
        if let Some(sender) = &self.cancel {
            // Remember cancellation across the wait -> installer handoff,
            // even after the wait future has dropped its receiver.
            sender.send_replace(true);
        }
        Ok(())
    }
    pub fn finish(&mut self, result: &Result<(), UiMessage>) {
        self.phase = Phase::Idle;
        self.cancel = None;
        if let Err(error) = result {
            self.status = Some(error.clone());
        }
    }
    pub fn allow_exit(&self, restart: bool) -> bool {
        if self.phase == Phase::Installing && !restart {
            return false;
        }
        // Cancel a waiting operation before allowing the event loop to close.
        let _ = self.cancel();
        true
    }
    pub fn enter_install(&mut self) -> Result<(T, Arc<Vec<u8>>), UiMessage> {
        if self.phase != Phase::Waiting {
            return Err(UiMessage::new("update.verify_first"));
        }
        if self.cancel.as_ref().is_some_and(|tx| *tx.borrow()) {
            return Err(UiMessage::new("update.cancel_install"));
        }
        let update = self
            .pending
            .clone()
            .ok_or(UiMessage::new("update.check_first"))?;
        let bytes = self
            .bytes
            .clone()
            .ok_or(UiMessage::new("update.verify_first"))?;
        self.phase = Phase::Installing;
        self.cancel = None;
        self.status = Some(UiMessage::new("update.installing"));
        Ok((update, bytes))
    }
    pub fn checked(&mut self, update: Option<T>, version: Option<String>) {
        self.status = Some(
            version
                .map(|v| UiMessage::new("update.found").arg("version", v))
                .unwrap_or_else(|| UiMessage::new("update.current")),
        );
        self.pending = update;
        self.bytes = None;
        self.received = 0;
        self.total = None;
    }
    pub fn progress(&mut self, chunk: usize, total: Option<u64>) {
        self.received = self.received.saturating_add(chunk as u64);
        self.total = total;
        if self.received > 512 * 1024 * 1024 || total.is_some_and(|n| n > 512 * 1024 * 1024) {
            self.limit_hit = true;
            let _ = self.cancel();
        }
        self.status = Some(
            UiMessage::new("update.received")
                .arg("size", format!("{:.1}", self.received as f64 / 1048576.0)),
        );
    }
    pub fn downloaded(&mut self, bytes: Vec<u8>) -> Result<(), UiMessage> {
        if self.limit_hit || bytes.len() > 512 * 1024 * 1024 {
            return Err(UiMessage::new("update.package_large"));
        }
        self.bytes = Some(Arc::new(bytes));
        self.status = Some(UiMessage::new("update.verified"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> State<String> {
        let mut state = State::new(true);
        state.checked(Some("N+1".into()), Some("2.0.0".into()));
        state.downloaded(vec![1, 2, 3]).unwrap();
        state
    }
    #[test]
    fn failed_install_retains_verified_bytes_and_allows_exact_retry() {
        let mut s = ready();
        let bytes = s.bytes.clone().unwrap();
        let _rx = s.begin(Phase::Waiting).unwrap();
        let (version, attempt) = s.enter_install().unwrap();
        assert_eq!(version, "N+1");
        assert!(Arc::ptr_eq(&bytes, &attempt));
        s.finish(&Err(UiMessage::new("update.install_failed")));
        assert!(!s.busy());
        assert!(Arc::ptr_eq(s.bytes.as_ref().unwrap(), &bytes));
        let _rx = s.begin(Phase::Waiting).unwrap();
        assert!(s.enter_install().is_ok());
    }
    #[test]
    fn cancellation_wins_until_install_and_never_claims_to_cancel_an_installer() {
        let mut s = ready();
        let _rx = s.begin(Phase::Waiting).unwrap();
        s.cancel().unwrap();
        assert!(s.enter_install().is_err());
        s.finish(&Err(UiMessage::new("update.cancel_install")));
        let _rx = s.begin(Phase::Waiting).unwrap();
        s.enter_install().unwrap();
        assert!(!s.cancellable());
        assert_eq!(s.cancel().unwrap_err().key, "update.cannot_cancel");
        assert!(s.begin(Phase::Checking).is_err());
    }
    #[test]
    fn operations_exclude_each_other_and_failed_checks_keep_previous_verified_package() {
        let mut s = ready();
        let _rx = s.begin(Phase::Checking).unwrap();
        assert!(s.begin(Phase::Downloading).is_err());
        s.finish(&Err(UiMessage::new("update.index_network")));
        assert!(s.pending.is_some() && s.bytes.is_some());
        let _rx = s.begin(Phase::Checking).unwrap();
        s.checked(None, None);
        s.finish(&Ok(()));
        assert!(s.bytes.is_none() && s.pending.is_none());
        assert!(s.begin(Phase::Downloading).is_err());
        assert!(s.begin(Phase::Waiting).is_err());
        assert!(!s.busy());
    }
    #[test]
    fn cancellation_survives_the_wait_receiver_being_dropped() {
        let mut s = ready();
        drop(s.begin(Phase::Waiting).unwrap());
        s.cancel().unwrap();
        assert_eq!(s.enter_install().unwrap_err().key, "update.cancel_install");
    }
    #[test]
    fn exit_cancels_a_wait_but_only_restart_may_exit_during_installation() {
        let mut s = ready();
        drop(s.begin(Phase::Waiting).unwrap());
        assert!(s.preserve_page());
        assert!(s.allow_exit(false));
        assert!(s.enter_install().is_err());
        s.finish(&Ok(()));
        assert!(!s.preserve_page());
        let _rx = s.begin(Phase::Waiting).unwrap();
        s.enter_install().unwrap();
        assert!(s.preserve_page());
        assert!(!s.allow_exit(false));
        assert!(s.allow_exit(true));
    }
    #[test]
    fn oversized_download_requests_cancel_and_never_becomes_installable() {
        let mut s = State::new(false);
        s.checked(Some("N+1"), Some("2.0.0".into()));
        let rx = s.begin(Phase::Downloading).unwrap();
        s.progress(12, Some(512 * 1024 * 1024 + 1));
        assert!(*rx.borrow());
        assert!(s.downloaded(vec![1]).is_err());
        assert!(s.bytes.is_none());
    }
}
