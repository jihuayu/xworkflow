pub mod pool_factories;
pub mod workflow_builders;

use tokio::runtime::Runtime;

use xworkflow::{FakeIdGenerator, FakeTimeProvider, RuntimeContext};

pub fn bench_context() -> RuntimeContext {
    let mut ctx = RuntimeContext::default();
    ctx.time_provider = std::sync::Arc::new(FakeTimeProvider::new(1_700_000_000));
    ctx.id_generator = std::sync::Arc::new(FakeIdGenerator::new("bench".into()));
    ctx
}

pub fn bench_runtime() -> Runtime {
    Runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("failed to build runtime")
}
