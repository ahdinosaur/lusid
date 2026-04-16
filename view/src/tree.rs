use serde::{Deserialize, Serialize};
use std::fmt::Display;
use termtree::Tree as TermTree;

use crate::View;

/// A nested tree of [`View`]s. Renders via [`termtree`] to an indented ASCII
/// tree (`├──`, `└──`, …) when printed with `Display`. Branch nodes have
/// their own `view` (the label shown at the branch) plus `children`; leaves
/// are just a `view`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ViewTree {
    Branch { view: View, children: Vec<ViewTree> },
    Leaf { view: View },
}

impl Display for ViewTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        TermTree::<View>::from(self.clone()).fmt(f)
    }
}

impl From<ViewTree> for TermTree<View> {
    fn from(value: ViewTree) -> Self {
        match value {
            ViewTree::Branch { view, children } => TermTree::new(view).with_leaves(children),
            ViewTree::Leaf { view } => TermTree::new(view),
        }
    }
}
