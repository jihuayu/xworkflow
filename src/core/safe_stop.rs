use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub struct SafeStopSignal {
    token: tokio_util::sync::CancellationToken,
    timeout: Arc<AtomicU64>,
}

impl SafeStopSignal {
    pub fn new() -> Self {
        Self {
            token: tokio_util::sync::CancellationToken::new(),
            timeout: Arc::new(AtomicU64::new(30)),
        }
    }

    pub fn trigger(&self, timeout_secs: u64) {
        self.timeout.store(timeout_secs, Ordering::Relaxed);
        self.token.cancel();
    }

    pub fn is_triggered(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn cancelled(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    pub fn timeout_secs(&self) -> u64 {
        self.timeout.load(Ordering::Relaxed)
    }
}

impl Default for SafeStopSignal {
    fn default() -> Self {
        Self::new()
    }
}
