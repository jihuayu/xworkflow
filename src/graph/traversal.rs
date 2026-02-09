use std::collections::HashSet;

use petgraph::stable_graph::StableDiGraph;

use super::types::{GraphEdge, GraphNode};

/// 获取所有就绪节点（所有前驱节点都已完成）
pub fn get_ready_nodes(
    graph: &StableDiGraph<GraphNode, GraphEdge>,
    completed_nodes: &HashSet<String>,
    node_index_map: &std::collections::HashMap<String, petgraph::stable_graph::NodeIndex>,
) -> Vec<String> {
    let mut ready = Vec::new();

    for idx in graph.node_indices() {
        if let Some(node) = graph.node_weight(idx) {
            // 跳过已完成的节点
            if completed_nodes.contains(&node.id) {
                continue;
            }

            // 检查所有前驱节点是否已完成
            let predecessors: Vec<_> = graph
                .neighbors_directed(idx, petgraph::Direction::Incoming)
                .collect();

            let all_deps_met = predecessors.iter().all(|pred_idx| {
                if let Some(pred_node) = graph.node_weight(*pred_idx) {
                    completed_nodes.contains(&pred_node.id)
                } else {
                    false
                }
            });

            // Start 节点（没有前驱）或者所有前驱已完成
            if all_deps_met {
                ready.push(node.id.clone());
            }
        }
    }

    // 去掉 node_index_map 的警告（保留用于未来扩展）
    let _ = node_index_map;

    ready
}

/// 拓扑排序
pub fn topological_sort(
    graph: &StableDiGraph<GraphNode, GraphEdge>,
) -> Option<Vec<String>> {
    let sorted = petgraph::algo::toposort(graph, None).ok()?;

    Some(
        sorted
            .into_iter()
            .filter_map(|idx| graph.node_weight(idx).map(|n| n.id.clone()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_topological_sort() {
        let mut graph = StableDiGraph::new();
        let a = graph.add_node(GraphNode {
            id: "start".to_string(),
            node_type: "start".to_string(),
            config: json!({}),
            title: "Start".to_string(),
        });
        let b = graph.add_node(GraphNode {
            id: "mid".to_string(),
            node_type: "template".to_string(),
            config: json!({}),
            title: "Mid".to_string(),
        });
        let c = graph.add_node(GraphNode {
            id: "end".to_string(),
            node_type: "end".to_string(),
            config: json!({}),
            title: "End".to_string(),
        });

        graph.add_edge(
            a,
            b,
            GraphEdge {
                id: "e1".to_string(),
                source: "start".to_string(),
                target: "mid".to_string(),
                edge_type: super::super::types::EdgeType::Normal,
            },
        );
        graph.add_edge(
            b,
            c,
            GraphEdge {
                id: "e2".to_string(),
                source: "mid".to_string(),
                target: "end".to_string(),
                edge_type: super::super::types::EdgeType::Normal,
            },
        );

        let sorted = topological_sort(&graph).unwrap();
        assert_eq!(sorted, vec!["start", "mid", "end"]);
    }

    #[test]
    fn test_get_ready_nodes() {
        let mut graph = StableDiGraph::new();
        let a = graph.add_node(GraphNode {
            id: "start".to_string(),
            node_type: "start".to_string(),
            config: json!({}),
            title: "Start".to_string(),
        });
        let b = graph.add_node(GraphNode {
            id: "mid".to_string(),
            node_type: "template".to_string(),
            config: json!({}),
            title: "Mid".to_string(),
        });
        let _c = graph.add_node(GraphNode {
            id: "end".to_string(),
            node_type: "end".to_string(),
            config: json!({}),
            title: "End".to_string(),
        });

        graph.add_edge(
            a,
            b,
            GraphEdge {
                id: "e1".to_string(),
                source: "start".to_string(),
                target: "mid".to_string(),
                edge_type: super::super::types::EdgeType::Normal,
            },
        );

        let map = HashMap::new();

        // Initially, only start (no predecessors) and end (no predecessors via edge) should be ready
        let completed = HashSet::new();
        let ready = get_ready_nodes(&graph, &completed, &map);
        assert!(ready.contains(&"start".to_string()));

        // After start completes, mid should be ready
        let mut completed = HashSet::new();
        completed.insert("start".to_string());
        let ready = get_ready_nodes(&graph, &completed, &map);
        assert!(ready.contains(&"mid".to_string()));
    }
}
