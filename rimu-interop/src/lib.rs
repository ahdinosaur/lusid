//! Conversion helpers between Rust types and [`rimu::Value`].
//!
//! - [`FromRimu`]: implement on a type to parse it from a Rimu value, with optional
//!   span-aware error reporting via `from_rimu_spanned`.
//! - [`FromRimuError`]: the unified error type returned by primitive impls and the
//!   `#[derive(FromRimu)]` macro. Recursive variants ([`FromRimuError::Field`],
//!   [`FromRimuError::Item`], …) carry an inner [`rimu::Spanned`] error so failures
//!   in nested structures preserve the source span of the offending value.
//! - [`to_rimu`]: serialize any `Serialize` type into a [`rimu::Spanned<Value>`]
//!   carrying a synthetic span (used for exposing Rust structs like `System` to
//!   plan scripts).

mod error;
mod from_rimu;
mod from_rimu_impls;
mod to_rimu;

pub use crate::error::*;
pub use crate::from_rimu::*;
pub use crate::to_rimu::*;

/// Derive `FromRimu` for a struct or tagged/untagged enum.
///
/// See `rimu-interop-macros` for supported attributes
/// (`#[rimu(tag = "...")]`, `#[rimu(untagged)]`, `#[rimu(rename_all = "...")]`,
/// `#[rimu(rename = "...")]`).
pub use rimu_interop_macros::FromRimu;
