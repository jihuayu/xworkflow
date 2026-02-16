use criterion::{criterion_group, criterion_main, Criterion};
use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};

mod helpers;

use helpers::workflow_builders::{
    build_branch_workflow, build_deep_branch_chain, build_diamond_workflow, build_fanout_workflow,
    build_linear_workflow,
};
use helpers::{bench_runtime, DispatcherSetup};

fn bench_dispatcher(c: &mut Criterion) {
    let rt = bench_runtime();

    c.bench_function("dispatch_start_end", |b| {
        let yaml = build_linear_workflow(1, "template-transform");
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt)
            .iter(|| async { setup.run_hot(VariablePool::new()).await });
    });

    for size in [5usize, 10, 50] {
        let name = format!("dispatch_linear_{}", size);
        let yaml = build_linear_workflow(size, "template-transform");
        let setup = DispatcherSetup::from_yaml(&yaml);
        c.bench_function(&name, |b| {
            b.to_async(&rt)
                .iter(|| async { setup.run_hot(VariablePool::new()).await });
        });
    }

    c.bench_function("dispatch_branch_2_way", |b| {
        let yaml = build_branch_workflow(2);
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(&Selector::new("start", "flag0"), Segment::Boolean(true));
            setup.run_hot(pool).await;
        });
    });

    c.bench_function("dispatch_branch_10_way", |b| {
        let yaml = build_fanout_workflow(10);
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(&Selector::new("start", "flag0"), Segment::Boolean(true));
            setup.run_hot(pool).await;
        });
    });

    c.bench_function("dispatch_diamond_10", |b| {
        let yaml = build_diamond_workflow(10);
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(&Selector::new("start", "flag0"), Segment::Boolean(true));
            pool.set(
                &Selector::new("start", "query"),
                Segment::String("bench".into()),
            );
            setup.run_hot(pool).await;
        });
    });

    c.bench_function("dispatch_deep_branch_10", |b| {
        let yaml = build_deep_branch_chain(10);
        let setup = DispatcherSetup::from_yaml(&yaml);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            for i in 1..=10 {
                pool.set(
                    &Selector::new("start", format!("flag{}", i)),
                    Segment::Boolean(true),
                );
            }
            setup.run_hot(pool).await;
        });
    });
}

criterion_group!(benches, bench_dispatcher);
criterion_main!(benches);
