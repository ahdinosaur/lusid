use displaydoc::Display;
use rimu::{SerdeValueError, SourceId, Span, Spanned, Value, to_serde_value};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, Error, Display)]
pub enum ToRimuError {
    /// Failed to convert to Rimu SerdeValue
    SerdeValue(#[from] SerdeValueError),
}

/// Serialize any `Serialize` type into a Rimu [`Spanned<Value>`].
///
/// The resulting value gets a synthetic zero-width span attributed to `source_id`,
/// since the data originated from Rust rather than a parsed source file. This is
/// how Rust-side structs (e.g. the detected `System` info) are injected into plan
/// scripts as function arguments.
pub fn to_rimu<T>(value: T, source_id: SourceId) -> Result<Spanned<Value>, ToRimuError>
where
    T: Serialize,
{
    let rimu_serde_value = to_serde_value(value).map_err(ToRimuError::from)?;
    let rimu_value = rimu_serde_value.with_span(Span::new(source_id, 0, 0));
    Ok(rimu_value)
}
