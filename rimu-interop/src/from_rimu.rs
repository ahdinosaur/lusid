use rimu::{Spanned, Value};

/// Parse a Rust type from a [`rimu::Value`].
///
/// Implementors define how to read their shape out of the dynamic Rimu value.
/// The provided `from_rimu_spanned` method lifts the conversion into span-aware
/// territory: the resulting `Spanned<Self>` retains the source span so downstream
/// diagnostics can point back to the exact plan location.
pub trait FromRimu {
    type Error: Clone;

    fn from_rimu(value: Value) -> Result<Self, Self::Error>
    where
        Self: Sized;

    /// Preserve source span on both success and failure, so error diagnostics can
    /// highlight the offending plan location.
    fn from_rimu_spanned(value: Spanned<Value>) -> Result<Spanned<Self>, Spanned<Self::Error>>
    where
        Self: Sized + Clone,
    {
        let (value, span) = value.take();
        match Self::from_rimu(value) {
            Ok(this) => Ok(Spanned::new(this, span)),
            Err(error) => Err(Spanned::new(error, span)),
        }
    }
}
