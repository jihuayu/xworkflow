use petgraph::stable_graph::StableDiGraph;

use crate::error::WorkflowError;

use super::types::{GraphEdge, GraphNode};

/// 验证图的合法性
pub fn validate_graph(
    graph: &StableDiGraph<GraphNode, GraphEdge>,
) -> Result<(), WorkflowError> {
    // 1. 检测环（DAG 验证）
    if petgraph::algo::is_cyclic_directed(graph) {
        return Err(WorkflowError::CycleDetected);
    }

    // 2. 检查孤立节点（没有入边也没有出边的非 start/end 节点）
    for idx in graph.node_indices() {
        if let Some(node) = graph.node_weight(idx) {
            if node.node_type != "start" && node.node_type != "end" {
                let in_degree = graph
                    .neighbors_directed(idx, petgraph::Direction::Incoming)
                    .count();
                let out_degree = graph
                    .neighbors_directed(idx, petgraph::Direction::Outgoing)
                    .count();

                if in_degree == 0 && out_degree == 0 {
                    return Err(WorkflowError::GraphValidationError(format!(
                        "Isolated node detected: {}",
                        node.id
                    )));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use petgraph::stable_graph::StableDiGraph;
    use serde_json::json;

    #[test]
    fn test_validate_dag() {
        let mut graph = StableDiGraph::new();
        let a = graph.add_node(GraphNode {
            id: "start".to_string(),
            node_type: "start".to_string(),
            config: json!({}),
            title: "Start".to_string(),
        });
        let b = graph.add_node(GraphNode {
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
                target: "end".to_string(),
                edge_type: super::super::types::EdgeType::Normal,
            },
        );

        assert!(validate_graph(&graph).is_ok());
    }

    #[test]
    fn test_detect_cycle() {
        let mut graph = StableDiGraph::new();
        let a = graph.add_node(GraphNode {
            id: "a".to_string(),
            node_type: "template".to_string(),
            config: json!({}),
            title: "A".to_string(),
        });
        let b = graph.add_node(GraphNode {
            id: "b".to_string(),
            node_type: "template".to_string(),
            config: json!({}),
            title: "B".to_string(),
        });

        let edge1 = GraphEdge {
            id: "e1".to_string(),
            source: "a".to_string(),
            target: "b".to_string(),
            edge_type: super::super::types::EdgeType::Normal,
        };
        let edge2 = GraphEdge {
            id: "e2".to_string(),
            source: "b".to_string(),
            target: "a".to_string(),
            edge_type: super::super::types::EdgeType::Normal,
        };

        graph.add_edge(a, b, edge1);
        graph.add_edge(b, a, edge2);

        assert!(matches!(
            validate_graph(&graph),
            Err(WorkflowError::CycleDetected)
        ));
    }
}
