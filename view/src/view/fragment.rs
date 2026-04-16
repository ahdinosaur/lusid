use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::View;

/// Concatenation of views with no separator or container. Useful for building
/// up a view incrementally or returning "nothing" (empty children) without
/// needing an `Option<View>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fragment {
    pub children: Vec<View>,
}

impl Display for Fragment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for children in self.children.iter() {
            Display::fmt(children, f)?
        }
        Ok(())
    }
}

impl Fragment {
    pub fn new(children: Vec<View>) -> Self {
        Self { children }
    }
}

impl From<Vec<View>> for Fragment {
    fn from(value: Vec<View>) -> Self {
        Fragment::new(value)
    }
}

impl From<Fragment> for View {
    fn from(value: Fragment) -> Self {
        View::Fragment(value)
    }
}

impl From<Vec<View>> for View {
    fn from(value: Vec<View>) -> Self {
        View::Fragment(value.into())
    }
}
