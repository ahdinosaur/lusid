use crate::{Fragment, View};

pub trait Render {
    fn render(&self) -> View;
}

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
