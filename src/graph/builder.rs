use std::collections::HashMap;

use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;

use crate::dsl::WorkflowSchema;
use crate::error::WorkflowError;

use super::types::*;

/// 工作流定义 - 从 DSL 解析后的不可变图结构
#[derive(Debug)]
pub struct WorkflowDefinition {
    /// 工作流唯一标识
    pub id: String,

    /// 工作流名称
    pub name: String,

    /// 图结构
    pub graph: StableDiGraph<GraphNode, GraphEdge>,

    /// 开始节点索引
    pub start_node_idx: NodeIndex,

    /// 结束节点 ID
    pub end_node_id: String,

    /// 节点 ID 到 NodeIndex 的映射
    pub node_index_map: NodeIndexMap,

    /// 元数据
    pub metadata: WorkflowMetadata,
}

impl WorkflowDefinition {
    /// 根据节点 ID 获取图节点
    pub fn get_node(&self, node_id: &str) -> Result<&GraphNode, WorkflowError> {
        let idx = self
            .node_index_map
            .get(node_id)
            .ok_or_else(|| WorkflowError::NodeNotFound(node_id.to_string()))?;
        self.graph
            .node_weight(*idx)
            .ok_or_else(|| WorkflowError::NodeNotFound(node_id.to_string()))
    }

    /// 获取节点的所有后继节点 ID
    pub fn get_successors(&self, node_id: &str) -> Result<Vec<String>, WorkflowError> {
        let idx = self
            .node_index_map
            .get(node_id)
            .ok_or_else(|| WorkflowError::NodeNotFound(node_id.to_string()))?;

        let successors: Vec<String> = self
            .graph
            .neighbors_directed(*idx, petgraph::Direction::Outgoing)
            .filter_map(|n| self.graph.node_weight(n).map(|node| node.id.clone()))
            .collect();

        Ok(successors)
    }

    /// 获取节点的所有前驱节点 ID
    pub fn get_predecessors(&self, node_id: &str) -> Result<Vec<String>, WorkflowError> {
        let idx = self
            .node_index_map
            .get(node_id)
            .ok_or_else(|| WorkflowError::NodeNotFound(node_id.to_string()))?;

        let predecessors: Vec<String> = self
            .graph
            .neighbors_directed(*idx, petgraph::Direction::Incoming)
            .filter_map(|n| self.graph.node_weight(n).map(|node| node.id.clone()))
            .collect();

        Ok(predecessors)
    }

    /// 获取 Start 节点 ID
    pub fn get_start_node_id(&self) -> String {
        self.graph
            .node_weight(self.start_node_idx)
            .map(|n| n.id.clone())
            .unwrap_or_default()
    }

    /// 获取 End 节点 ID
    pub fn get_end_node_id(&self) -> Result<String, WorkflowError> {
        Ok(self.end_node_id.clone())
    }

    /// 获取从某节点出发的特定类型的边的目标节点
    pub fn get_successor_by_edge_type(
        &self,
        node_id: &str,
        edge_type: &EdgeType,
    ) -> Result<Option<String>, WorkflowError> {
        let idx = self
            .node_index_map
            .get(node_id)
            .ok_or_else(|| WorkflowError::NodeNotFound(node_id.to_string()))?;

        let edges = self
            .graph
            .edges_directed(*idx, petgraph::Direction::Outgoing);

        for edge in edges {
            if &edge.weight().edge_type == edge_type {
                if let Some(target_node) = self.graph.node_weight(edge.target()) {
                    return Ok(Some(target_node.id.clone()));
                }
            }
        }

        Ok(None)
    }
}

/// 从 DSL schema 构建工作流定义
pub fn build_graph(dsl: &WorkflowSchema) -> Result<WorkflowDefinition, WorkflowError> {
    let mut graph = StableDiGraph::<GraphNode, GraphEdge>::new();
    let mut node_index_map: HashMap<String, NodeIndex> = HashMap::new();

    // 1. 添加所有节点
    for node_schema in &dsl.nodes {
        let graph_node = GraphNode {
            id: node_schema.id.clone(),
            node_type: node_schema.node_type.clone(),
            config: node_schema.data.clone(),
            title: if node_schema.title.is_empty() {
                node_schema.id.clone()
            } else {
                node_schema.title.clone()
            },
        };

        let idx = graph.add_node(graph_node);
        node_index_map.insert(node_schema.id.clone(), idx);
    }

    // 2. 添加所有边
    for edge_schema in &dsl.edges {
        let source_idx = node_index_map
            .get(&edge_schema.source)
            .ok_or_else(|| {
                WorkflowError::GraphBuildError(format!(
                    "Source node not found: {}",
                    edge_schema.source
                ))
            })?;

        let target_idx = node_index_map
            .get(&edge_schema.target)
            .ok_or_else(|| {
                WorkflowError::GraphBuildError(format!(
                    "Target node not found: {}",
                    edge_schema.target
                ))
            })?;

        let edge_type = EdgeType::from_source_handle(&edge_schema.source_handle);

        let graph_edge = GraphEdge {
            id: edge_schema.id.clone(),
            source: edge_schema.source.clone(),
            target: edge_schema.target.clone(),
            edge_type,
        };

        graph.add_edge(*source_idx, *target_idx, graph_edge);
    }

    // 3. 查找 Start 节点
    let start_node_idx = dsl
        .nodes
        .iter()
        .find(|n| n.node_type == "start")
        .and_then(|n| node_index_map.get(&n.id))
        .copied()
        .ok_or(WorkflowError::NoStartNode)?;

    // 4. 查找 End 节点 ID
    let end_node_id = dsl
        .nodes
        .iter()
        .find(|n| n.node_type == "end")
        .map(|n| n.id.clone())
        .ok_or(WorkflowError::NoEndNode)?;

    // 5. 构建元数据
    let metadata = WorkflowMetadata {
        variables: dsl.variables.clone(),
        environment_variables: dsl.environment_variables.clone(),
        conversation_variables: dsl.conversation_variables.clone(),
    };

    Ok(WorkflowDefinition {
        id: uuid::Uuid::new_v4().to_string(),
        name: "workflow".to_string(),
        graph,
        start_node_idx,
        end_node_id,
        node_index_map,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::*;
    use serde_json::json;

    fn create_simple_dsl() -> WorkflowSchema {
        WorkflowSchema {
            nodes: vec![
                NodeSchema {
                    id: "start".to_string(),
                    node_type: "start".to_string(),
                    data: json!({}),
                    title: "Start".to_string(),
                    position: None,
                },
                NodeSchema {
                    id: "end".to_string(),
                    node_type: "end".to_string(),
                    data: json!({}),
                    title: "End".to_string(),
                    position: None,
                },
            ],
            edges: vec![EdgeSchema {
                id: "e1".to_string(),
                source: "start".to_string(),
                target: "end".to_string(),
                source_handle: None,
            }],
            variables: vec![],
            environment_variables: vec![],
            conversation_variables: vec![],
        }
    }

    #[test]
    fn test_build_simple_graph() {
        let dsl = create_simple_dsl();
        let def = build_graph(&dsl).unwrap();

        assert_eq!(def.get_start_node_id(), "start");
        assert_eq!(def.get_end_node_id().unwrap(), "end");
    }

    #[test]
    fn test_get_successors() {
        let dsl = create_simple_dsl();
        let def = build_graph(&dsl).unwrap();

        let successors = def.get_successors("start").unwrap();
        assert_eq!(successors, vec!["end"]);
    }

    #[test]
    fn test_get_predecessors() {
        let dsl = create_simple_dsl();
        let def = build_graph(&dsl).unwrap();

        let predecessors = def.get_predecessors("end").unwrap();
        assert_eq!(predecessors, vec!["start"]);
    }

    #[test]
    fn test_get_node() {
        let dsl = create_simple_dsl();
        let def = build_graph(&dsl).unwrap();

        let node = def.get_node("start").unwrap();
        assert_eq!(node.node_type, "start");
    }

    #[test]
    fn test_branching_graph() {
        let dsl = WorkflowSchema {
            nodes: vec![
                NodeSchema {
                    id: "start".to_string(),
                    node_type: "start".to_string(),
                    data: json!({}),
                    title: "".to_string(),
                    position: None,
                },
                NodeSchema {
                    id: "if1".to_string(),
                    node_type: "if-else".to_string(),
                    data: json!({}),
                    title: "".to_string(),
                    position: None,
                },
                NodeSchema {
                    id: "branch_a".to_string(),
                    node_type: "template".to_string(),
                    data: json!({}),
                    title: "".to_string(),
                    position: None,
                },
                NodeSchema {
                    id: "branch_b".to_string(),
                    node_type: "template".to_string(),
                    data: json!({}),
                    title: "".to_string(),
                    position: None,
                },
                NodeSchema {
                    id: "end".to_string(),
                    node_type: "end".to_string(),
                    data: json!({}),
                    title: "".to_string(),
                    position: None,
                },
            ],
            edges: vec![
                EdgeSchema {
                    id: "e1".to_string(),
                    source: "start".to_string(),
                    target: "if1".to_string(),
                    source_handle: None,
                },
                EdgeSchema {
                    id: "e2".to_string(),
                    source: "if1".to_string(),
                    target: "branch_a".to_string(),
                    source_handle: Some("true".to_string()),
                },
                EdgeSchema {
                    id: "e3".to_string(),
                    source: "if1".to_string(),
                    target: "branch_b".to_string(),
                    source_handle: Some("false".to_string()),
                },
                EdgeSchema {
                    id: "e4".to_string(),
                    source: "branch_a".to_string(),
                    target: "end".to_string(),
                    source_handle: None,
                },
                EdgeSchema {
                    id: "e5".to_string(),
                    source: "branch_b".to_string(),
                    target: "end".to_string(),
                    source_handle: None,
                },
            ],
            variables: vec![],
            environment_variables: vec![],
            conversation_variables: vec![],
        };

        let def = build_graph(&dsl).unwrap();

        // if1 节点应该有两个后继
        let successors = def.get_successors("if1").unwrap();
        assert_eq!(successors.len(), 2);

        // 通过边类型获取分支
        let true_branch = def
            .get_successor_by_edge_type("if1", &EdgeType::TrueBranch)
            .unwrap();
        assert_eq!(true_branch, Some("branch_a".to_string()));

        let false_branch = def
            .get_successor_by_edge_type("if1", &EdgeType::FalseBranch)
            .unwrap();
        assert_eq!(false_branch, Some("branch_b".to_string()));
    }
}
