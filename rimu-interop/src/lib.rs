//! Conversion helpers between Rust types and [`rimu::Value`].
//!
//! - [`FromRimu`]: implement on a type to parse it from a Rimu value, with optional
//!   span-aware error reporting via `from_rimu_spanned`.
//! - [`to_rimu`]: serialize any `Serialize` type into a [`rimu::Spanned<Value>`]
//!   carrying a synthetic span (used for exposing Rust structs like `System` to
//!   plan scripts).

mod from_rimu;
mod to_rimu;

pub use crate::from_rimu::*;
pub use crate::to_rimu::*;
