use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::core::event_bus::GraphEngineEvent;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Runtime context providing time and ID generation
#[derive(Clone)]
pub struct RuntimeContext {
    pub time_provider: Arc<dyn TimeProvider>,
    pub id_generator: Arc<dyn IdGenerator>,
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
}

impl Default for RuntimeContext {
    fn default() -> Self {
        Self {
            time_provider: Arc::new(RealTimeProvider::default()),
            id_generator: Arc::new(RealIdGenerator::default()),
            event_tx: None,
        }
    }
}

impl RuntimeContext {
    pub fn with_event_tx(mut self, event_tx: mpsc::Sender<GraphEngineEvent>) -> Self {
        self.event_tx = Some(event_tx);
        self
    }
}

pub trait TimeProvider: Send + Sync {
    fn now_timestamp(&self) -> i64;
    fn now_millis(&self) -> i64;
    fn elapsed_secs(&self, since: i64) -> u64;
}

pub trait IdGenerator: Send + Sync {
    fn next_id(&self) -> String;
}

// --- Real implementations ---

pub struct RealTimeProvider {
    #[allow(dead_code)]
    start: Instant,
}

impl RealTimeProvider {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for RealTimeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeProvider for RealTimeProvider {
    fn now_timestamp(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    fn now_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    fn elapsed_secs(&self, since: i64) -> u64 {
        let now = self.now_timestamp();
        if now >= since {
            (now - since) as u64
        } else {
            0
        }
    }
}

pub struct RealIdGenerator;

impl Default for RealIdGenerator {
    fn default() -> Self {
        Self
    }
}

impl IdGenerator for RealIdGenerator {
    fn next_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

// --- Fake implementations ---

pub struct FakeTimeProvider {
    pub fixed_timestamp: i64,
}

impl FakeTimeProvider {
    pub fn new(fixed_timestamp: i64) -> Self {
        Self { fixed_timestamp }
    }
}

impl TimeProvider for FakeTimeProvider {
    fn now_timestamp(&self) -> i64 {
        self.fixed_timestamp
    }

    fn now_millis(&self) -> i64 {
        self.fixed_timestamp.saturating_mul(1000)
    }

    fn elapsed_secs(&self, since: i64) -> u64 {
        if self.fixed_timestamp >= since {
            (self.fixed_timestamp - since) as u64
        } else {
            0
        }
    }
}

pub struct FakeIdGenerator {
    pub prefix: String,
    pub counter: AtomicU64,
}

impl FakeIdGenerator {
    pub fn new(prefix: String) -> Self {
        Self {
            prefix,
            counter: AtomicU64::new(0),
        }
    }
}

impl IdGenerator for FakeIdGenerator {
    fn next_id(&self) -> String {
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        format!("{}-{}", self.prefix, id)
    }
}
