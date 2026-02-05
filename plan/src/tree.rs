use cuid2::create_id;
use lusid_causality::CausalityMeta;
use lusid_tree::{FlatTree, FlatTreeNode, Tree};
use lusid_view::{Render, ViewTree};

use crate::PlanNodeId;

pub type PlanTree<Node> = Tree<Node, PlanMeta>;
pub type PlanMeta = CausalityMeta<PlanNodeId>;
pub type PlanFlatTree<Node> = FlatTree<Node, PlanMeta>;
pub type PlanFlatTreeNode<Node> = FlatTreeNode<Node, PlanMeta>;

pub fn map_plan_subitems<Node, NextNode, MapFn, MapFnIter>(
    node: Node,
    map: MapFn,
) -> impl Iterator<Item = PlanTree<NextNode>>
where
    MapFn: Fn(Node) -> MapFnIter,
    MapFnIter: IntoIterator<Item = Tree<NextNode, CausalityMeta<String>>>,
{
    let scope_id = create_id();
    map(node).into_iter().map(move |tree| {
        tree.map_meta(|meta| CausalityMeta {
            id: meta.id.map(|item_id| PlanNodeId::SubItem {
                scope_id: scope_id.clone(),
                item_id,
            }),
            before: meta
                .before
                .into_iter()
                .map(|item_id| PlanNodeId::SubItem {
                    scope_id: scope_id.clone(),
                    item_id,
                })
                .collect(),
            after: meta
                .after
                .into_iter()
                .map(|item_id| PlanNodeId::SubItem {
                    scope_id: scope_id.clone(),
                    item_id,
                })
                .collect(),
        })
    })
}

pub fn render_plan_tree<Node>(tree: PlanTree<Node>) -> ViewTree
where
    Node: Render,
{
    match tree {
        Tree::Branch { meta, children } => ViewTree::Branch {
            view: meta.id.map(|id| id.render()).unwrap_or(".".render()),
            children: children.into_iter().map(render_plan_tree).collect(),
        },
        Tree::Leaf { meta: _, node } => ViewTree::Leaf {
            view: node.render(),
        },
    }
}
