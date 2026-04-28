//! Unified error type for [`crate::FromRimu`] impls.

use displaydoc::Display;
use rimu::{Span, Spanned, Value};
use thiserror::Error;

/// Errors produced by [`crate::FromRimu`] conversions.
///
/// One enum is shared across primitive impls, generated `#[derive(FromRimu)]`
/// impls, and downstream user impls. The recursive variants ([`Self::Field`],
/// [`Self::Item`], [`Self::ObjectValue`], [`Self::NoVariantMatched`]) carry an
/// inner [`Spanned`] error, so a failure deep in a nested structure preserves
/// the exact source span of the offending value.
#[derive(Debug, Clone, Error, Display)]
pub enum FromRimuError {
    /// expected {expected}, got {got:?}
    WrongType {
        expected: &'static str,
        got: Box<Value>,
    },

    /// missing required field "{name}"
    MissingField { name: &'static str },

    /// unknown field "{name}"
    UnknownField { name: String, span: Span },

    /// missing discriminant field "{tag}"
    MissingDiscriminant { tag: &'static str },

    /// expected discriminant field "{tag}" to be a string, got {got:?}
    DiscriminantNotAString {
        tag: &'static str,
        span: Span,
        got: Box<Value>,
    },

    /// unknown variant "{value}" for tag "{tag}"
    UnknownVariant {
        tag: &'static str,
        span: Span,
        value: String,
    },

    /// no variant matched the value
    NoVariantMatched {
        case_errors: Vec<Spanned<FromRimuError>>,
    },

    /// invalid field "{name}": {error}
    Field {
        name: String,
        error: Box<Spanned<FromRimuError>>,
    },

    /// invalid list item at index {index}: {error}
    Item {
        index: usize,
        error: Box<Spanned<FromRimuError>>,
    },

    /// invalid object value at key "{key}": {error}
    ObjectValue {
        key: String,
        error: Box<Spanned<FromRimuError>>,
    },

    /// number out of range for {target}
    NumberOutOfRange { target: &'static str },
}
