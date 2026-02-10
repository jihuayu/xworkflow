#![allow(dead_code)]

use std::collections::HashMap;

use xworkflow::core::variable_pool::{Segment, VariablePool};

pub fn make_pool_with_strings(count: usize, string_size: usize) -> VariablePool {
    let mut pool = VariablePool::new();
    let value = "x".repeat(string_size);
    for i in 0..count {
        pool.set(
            &["node".to_string(), format!("key{}", i)],
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
            &["node".to_string(), format!("obj{}", i)],
            Segment::Object(obj),
        );
    }
    pool
}

pub fn make_realistic_pool(node_count: usize) -> VariablePool {
    let mut pool = VariablePool::new();
    for n in 0..node_count {
        pool.set(
            &[format!("node{}", n), "status".to_string()],
            Segment::String("ok".into()),
        );
        pool.set(
            &[format!("node{}", n), "value".to_string()],
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
