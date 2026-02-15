use lusid_tree::Tree;

pub type CausalityTree<Node, NodeId = String> = Tree<Node, CausalityMeta<NodeId>>;

#[derive(Debug, Clone)]
pub struct CausalityMeta<NodeId> {
    pub id: Option<NodeId>,
    pub requires: Vec<NodeId>,
    pub required_by: Vec<NodeId>,
}

impl<NodeId> Default for CausalityMeta<NodeId> {
    fn default() -> Self {
        Self {
            id: None,
            requires: Vec::new(),
            required_by: Vec::new(),
        }
    }
}

impl<NodeId> CausalityMeta<NodeId> {
    pub fn id(id: NodeId) -> Self {
        Self {
            id: Some(id),
            requires: vec![],
            required_by: vec![],
        }
    }

    pub fn requires(requires: Vec<NodeId>) -> Self {
        Self {
            id: None,
            requires,
            required_by: vec![],
        }
    }

    pub fn required_by(required_by: Vec<NodeId>) -> Self {
        Self {
            id: None,
            requires: vec![],
            required_by,
        }
    }
}
