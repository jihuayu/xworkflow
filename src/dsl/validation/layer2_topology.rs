use std::collections::{HashMap, HashSet, VecDeque};

use crate::dsl::schema::WorkflowSchema;

use super::types::{Diagnostic, DiagnosticLevel};

pub struct TopologyInfo {
    pub reachable: HashSet<String>,
    pub topo_level: HashMap<String, usize>,
}

pub fn validate(schema: &WorkflowSchema) -> (Vec<Diagnostic>, TopologyInfo) {
    let mut diags = Vec::new();
    let mut out_edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut in_edges: HashMap<String, Vec<String>> = HashMap::new();

    for node in &schema.nodes {
        out_edges.entry(node.id.clone()).or_default();
        in_edges.entry(node.id.clone()).or_default();
    }

    for edge in &schema.edges {
        out_edges.entry(edge.source.clone()).or_default().push(edge.target.clone());
        in_edges.entry(edge.target.clone()).or_default().push(edge.source.clone());
    }

    if let Some(start_node) = schema.nodes.iter().find(|n| n.data.node_type == "start") {
        let start_id = start_node.id.clone();

        if let Some(incoming) = in_edges.get(&start_id) {
            if !incoming.is_empty() {
                diags.push(error(
                    "E104",
                    "Start node has incoming edges".to_string(),
                    Some(start_id.clone()),
                ));
            }
        }

        let (reachable, topo_level) = bfs_reachable(&start_id, &out_edges);

        for node in &schema.nodes {
            if !reachable.contains(&node.id) {
                diags.push(error(
                    "E102",
                    format!("Unreachable node: {}", node.id),
                    Some(node.id.clone()),
                ));
            }
        }

        let has_end = schema
            .nodes
            .iter()
            .any(|n| (n.data.node_type == "end" || n.data.node_type == "answer") && reachable.contains(&n.id));
        if !has_end {
            diags.push(error(
                "E103",
                "No path from start to end/answer".to_string(),
                None,
            ));
        }

        for node in &schema.nodes {
            if (node.data.node_type == "end" || node.data.node_type == "answer")
                && out_edges.get(&node.id).map(|v| !v.is_empty()).unwrap_or(false)
            {
                diags.push(warn(
                    "W101",
                    "End/answer node has outgoing edges".to_string(),
                    Some(node.id.clone()),
                ));
            }
        }

        for edge in &schema.edges {
            if !reachable.contains(&edge.source) || !reachable.contains(&edge.target) {
                diags.push(warn(
                    "W102",
                    "Orphan edge not on any reachable path".to_string(),
                    None,
                ));
            }
        }

        let mut cycles = detect_cycles(&out_edges);
        diags.append(&mut cycles);

        return (
            diags,
            TopologyInfo {
                reachable,
                topo_level,
            },
        );
    }

    (
        diags,
        TopologyInfo {
            reachable: HashSet::new(),
            topo_level: HashMap::new(),
        },
    )
}

fn bfs_reachable(
    start: &str,
    out_edges: &HashMap<String, Vec<String>>,
) -> (HashSet<String>, HashMap<String, usize>) {
    let mut reachable = HashSet::new();
    let mut level = HashMap::new();
    let mut queue = VecDeque::new();
    reachable.insert(start.to_string());
    level.insert(start.to_string(), 0);
    queue.push_back(start.to_string());

    while let Some(node) = queue.pop_front() {
        let next_level = level.get(&node).copied().unwrap_or(0) + 1;
        if let Some(nexts) = out_edges.get(&node) {
            for n in nexts {
                if !reachable.contains(n) {
                    reachable.insert(n.clone());
                    level.insert(n.clone(), next_level);
                    queue.push_back(n.clone());
                }
            }
        }
    }

    (reachable, level)
}

fn detect_cycles(out_edges: &HashMap<String, Vec<String>>) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut state: HashMap<String, u8> = HashMap::new();
    let mut stack: Vec<String> = Vec::new();

    for node in out_edges.keys() {
        state.entry(node.clone()).or_insert(0);
    }

    for node in out_edges.keys() {
        if *state.get(node).unwrap_or(&0) == 0 {
            dfs(node, out_edges, &mut state, &mut stack, &mut diags);
        }
    }

    diags
}

fn dfs(
    node: &str,
    out_edges: &HashMap<String, Vec<String>>,
    state: &mut HashMap<String, u8>,
    stack: &mut Vec<String>,
    diags: &mut Vec<Diagnostic>,
) {
    state.insert(node.to_string(), 1);
    stack.push(node.to_string());

    if let Some(nexts) = out_edges.get(node) {
        for next in nexts {
            match state.get(next).copied().unwrap_or(0) {
                0 => dfs(next, out_edges, state, stack, diags),
                1 => {
                    if let Some(pos) = stack.iter().position(|n| n == next) {
                        let cycle = stack[pos..].to_vec();
                        let mut path = cycle.clone();
                        path.push(next.clone());
                        diags.push(error(
                            "E101",
                            format!("Cycle detected: {}", path.join(" -> ")),
                            Some(next.clone()),
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    stack.pop();
    state.insert(node.to_string(), 2);
}

fn error(code: &str, message: String, node_id: Option<String>) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Error,
        code: code.to_string(),
        message,
        node_id,
        edge_id: None,
        field_path: None,
    }
}

fn warn(code: &str, message: String, node_id: Option<String>) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Warning,
        code: code.to_string(),
        message,
        node_id,
        edge_id: None,
        field_path: None,
    }
}
