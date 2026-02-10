use criterion::{criterion_group, criterion_main, Criterion};

use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};

mod helpers;

use helpers::workflow_builders::{
    build_deep_branch_chain, build_diamond_workflow, build_fanout_workflow,
    build_linear_workflow, build_realistic_mixed_workflow,
};
use helpers::{bench_runtime, DispatcherSetup};

fn bench_macro_topology(c: &mut Criterion) {
    let rt = bench_runtime();

    c.bench_function("topo_minimal", |b| {
        let yaml = build_linear_workflow(1, "template-transform");
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt)
            .iter(|| async { setup.run_hot(VariablePool::new()).await });
    });

    for size in [5usize, 10] {
        let name = format!("topo_linear_{}", size);
        let yaml = build_linear_workflow(size, "template-transform");
        let setup = DispatcherSetup::from_yaml(&yaml);
        c.bench_function(&name, |b| {
            b.to_async(&rt)
                .iter(|| async { setup.run_hot(VariablePool::new()).await });
        });
    }

    c.bench_function("topo_branch_2_way", |b| {
        let yaml = build_fanout_workflow(2);
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt)
            .iter(|| async { setup.run_hot(VariablePool::new()).await });
    });

    c.bench_function("topo_branch_5_way", |b| {
        let yaml = build_fanout_workflow(5);
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt)
            .iter(|| async { setup.run_hot(VariablePool::new()).await });
    });

    c.bench_function("topo_diamond_5", |b| {
        let yaml = build_diamond_workflow(5);
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(&Selector::new("start", "query"), Segment::String("bench".into()));
            pool.set(&Selector::new("start", "flag0"), Segment::Boolean(true));
            setup.run_hot(pool).await;
        });
    });

    c.bench_function("topo_diamond_10", |b| {
        let yaml = build_diamond_workflow(10);
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(&Selector::new("start", "query"), Segment::String("bench".into()));
            pool.set(&Selector::new("start", "flag0"), Segment::Boolean(true));
            setup.run_hot(pool).await;
        });
    });

    c.bench_function("topo_deep_branch_5", |b| {
        let yaml = build_deep_branch_chain(5);
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            for i in 1..=5 {
                pool.set(
                    &Selector::new("start", format!("flag{}", i)),
                    Segment::Boolean(true),
                );
            }
            setup.run_hot(pool).await;
        });
    });

    c.bench_function("topo_mixed_realistic", |b| {
        let yaml = build_realistic_mixed_workflow();
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(&Selector::new("start", "query"), Segment::String("world".into()));
            pool.set(&Selector::new("start", "flag"), Segment::Boolean(true));
            setup.run_hot(pool).await;
        });
    });
}

criterion_group!(benches, bench_macro_topology);
criterion_main!(benches);
