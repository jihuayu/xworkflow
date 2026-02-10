#![allow(dead_code)]

use std::collections::HashMap;

use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};

pub fn make_pool_with_strings(count: usize, string_size: usize) -> VariablePool {
    let mut pool = VariablePool::new();
    let value = "x".repeat(string_size);
    for i in 0..count {
        pool.set(
            &Selector::new("node", format!("key{}", i)),
            Segment::String(value.clone()),
        );
    }
    pool
}

pub fn make_pool_with_objects(count: usize, depth: usize) -> VariablePool {
    let mut pool = VariablePool::new();
    for i in 0..count {
        let obj = build_object(depth);
        pool.set(
            &Selector::new("node", format!("obj{}", i)),
            Segment::Object(obj),
        );
    }
    pool
}

pub fn make_realistic_pool(node_count: usize) -> VariablePool {
    let mut pool = VariablePool::new();
    for n in 0..node_count {
        pool.set(
            &Selector::new(format!("node{}", n), "status"),
            Segment::String("ok".into()),
        );
        pool.set(
            &Selector::new(format!("node{}", n), "value"),
            Segment::Integer(n as i64),
        );
    }
    pool
}

fn build_object(depth: usize) -> HashMap<String, Segment> {
    if depth == 0 {
        return HashMap::new();
    }
    let mut map = HashMap::new();
    map.insert("value".into(), Segment::String("data".into()));
    map.insert("nested".into(), Segment::Object(build_object(depth - 1)));
    map
}
