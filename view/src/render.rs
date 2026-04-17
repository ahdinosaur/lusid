use crate::{Fragment, View};

/// Produce a [`View`] for display. The main way domain types participate in
/// the view system; most implementations return a [`Line`](crate::Line) or a
/// [`Fragment`](crate::Fragment).
pub trait Render {
    fn render(&self) -> View;
}

/// `None` renders as an empty [`Fragment`] so optional fields can be rendered
/// unconditionally without special-casing.
impl<T> Render for Option<T>
where
    T: Render,
{
    fn render(&self) -> View {
        match self {
            Some(inner) => inner.render(),
            None => View::Fragment(Fragment::new(vec![])),
        }
    }
}

/// Blanket-impl [`Render`] for any `Display` type by wrapping `to_string()`
/// in a single [`Line`](crate::Line). Used from downstream crates to attach
/// [`Render`] to their own types without orphan rule contortions.
#[macro_export]
macro_rules! impl_display_render {
    ($type:ty) => {
        impl $crate::Render for $type {
            fn render(&self) -> $crate::View {
                $crate::View::Line(self.to_string().into())
            }
        }
    };
}

impl_display_render!(String);
impl_display_render!(&str);
