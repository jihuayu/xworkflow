use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use xworkflow::core::variable_pool::{Segment, SegmentArray, SegmentObject};

fn bench_segment(c: &mut Criterion) {
    c.bench_function("segment_to_value_string", |b| {
        let seg = Segment::String("hello".into());
        b.iter(|| {
            let _ = black_box(seg.to_value());
        });
    });

    c.bench_function("segment_to_value_nested_object", |b| {
        let seg = Segment::Object(Arc::new(SegmentObject::new({
            let mut map = std::collections::HashMap::new();
            map.insert(
                "level1".into(),
                Segment::Object(Arc::new(SegmentObject::new({
                    let mut inner = std::collections::HashMap::new();
                    inner.insert("level2".into(), Segment::String("value".into()));
                    inner
                }))),
            );
            map
        })));
        b.iter(|| {
            let _ = black_box(seg.to_value());
        });
    });

    c.bench_function("segment_to_value_array_1000", |b| {
        let seg = Segment::Array(Arc::new(SegmentArray::new(
            (0..1000).map(Segment::Integer).collect(),
        )));
        b.iter(|| {
            let _ = black_box(seg.to_value());
        });
    });

    c.bench_function("segment_from_value_string", |b| {
        let val = serde_json::json!("hello");
        b.iter(|| {
            let _ = black_box(Segment::from_value(&val));
        });
    });

    c.bench_function("segment_from_value_nested_object", |b| {
        let val = serde_json::json!({"a": {"b": {"c": 1}}});
        b.iter(|| {
            let _ = black_box(Segment::from_value(&val));
        });
    });

    c.bench_function("segment_from_value_array_1000", |b| {
        let val = serde_json::json!((0..1000).collect::<Vec<i32>>());
        b.iter(|| {
            let _ = black_box(Segment::from_value(&val));
        });
    });
}

criterion_group!(benches, bench_segment);
criterion_main!(benches);
