//! [`crate::FromRimu`] impls for primitive and standard-library types.
//!
//! These are the "leaf" conversions — the building blocks the
//! `#[derive(FromRimu)]` macro composes when generating impls for user-defined
//! structs and enums. Errors all unify under [`crate::FromRimuError`] so
//! generated code can wrap inner errors without juggling generics.
//!
//! Container impls ([`Vec`], [`indexmap::IndexMap`], [`Option`]) override
//! [`crate::FromRimu::from_rimu_spanned`] so per-item / per-entry diagnostics
//! point at the actual offending value's span rather than the outer container.

use std::path::PathBuf;

use indexmap::IndexMap;
use rimu::{Number, Spanned, Value};
use rust_decimal::prelude::ToPrimitive;

use crate::{FromRimu, FromRimuError};

/// Build a [`FromRimuError::WrongType`] error from an unexpected value.
fn wrong_type(expected: &'static str, got: Value) -> FromRimuError {
    FromRimuError::WrongType {
        expected,
        got: Box::new(got),
    }
}

impl FromRimu for Value {
    type Error = FromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        Ok(value)
    }
}

impl FromRimu for String {
    type Error = FromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::String(s) => Ok(s),
            other => Err(wrong_type("a string", other)),
        }
    }
}

impl FromRimu for bool {
    type Error = FromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Boolean(b) => Ok(b),
            other => Err(wrong_type("a boolean", other)),
        }
    }
}

impl FromRimu for Number {
    type Error = FromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Number(n) => Ok(n),
            other => Err(wrong_type("a number", other)),
        }
    }
}

/// Generate a [`FromRimu`] impl for a primitive numeric type by delegating
/// extraction to a `to_*` method on [`rust_decimal::Decimal`].
macro_rules! impl_from_rimu_numeric {
    ($($ty:ty => $to:ident, $name:literal);+ $(;)?) => {
        $(
            impl FromRimu for $ty {
                type Error = FromRimuError;

                fn from_rimu(value: Value) -> Result<Self, Self::Error> {
                    match value {
                        Value::Number(n) => (*n)
                            .$to()
                            .ok_or(FromRimuError::NumberOutOfRange { target: $name }),
                        other => Err(wrong_type("a number", other)),
                    }
                }
            }
        )+
    };
}

impl_from_rimu_numeric! {
    i8    => to_i8,    "i8";
    i16   => to_i16,   "i16";
    i32   => to_i32,   "i32";
    i64   => to_i64,   "i64";
    isize => to_isize, "isize";
    u8    => to_u8,    "u8";
    u16   => to_u16,   "u16";
    u32   => to_u32,   "u32";
    u64   => to_u64,   "u64";
    usize => to_usize, "usize";
    f32   => to_f32,   "f32";
    f64   => to_f64,   "f64";
}

impl FromRimu for PathBuf {
    type Error = FromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::HostPath(path) => Ok(path),
            other => Err(wrong_type("a host-path", other)),
        }
    }
}

/// `Option<T>` reads as `None` when the value is [`Value::Null`], otherwise
/// delegates to `T::from_rimu`. The struct/enum derive separately handles the
/// "field missing entirely" case before this impl is consulted.
impl<T> FromRimu for Option<T>
where
    T: FromRimu<Error = FromRimuError> + Clone,
{
    type Error = FromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Null => Ok(None),
            other => T::from_rimu(other).map(Some),
        }
    }

    fn from_rimu_spanned(value: Spanned<Value>) -> Result<Spanned<Self>, Spanned<Self::Error>> {
        let span = value.span();
        match value.inner() {
            Value::Null => Ok(Spanned::new(None, span)),
            _ => {
                let inner = T::from_rimu_spanned(value)?;
                let (inner, span) = inner.take();
                Ok(Spanned::new(Some(inner), span))
            }
        }
    }
}

impl<T> FromRimu for Vec<T>
where
    T: FromRimu<Error = FromRimuError> + Clone,
{
    type Error = FromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::List(items) => items
                .into_iter()
                .enumerate()
                .map(|(index, item)| {
                    T::from_rimu_spanned(item)
                        .map(Spanned::into_inner)
                        .map_err(|error| FromRimuError::Item {
                            index,
                            error: Box::new(error),
                        })
                })
                .collect(),
            other => Err(wrong_type("a list", other)),
        }
    }
}

impl<T> FromRimu for IndexMap<String, T>
where
    T: FromRimu<Error = FromRimuError> + Clone,
{
    type Error = FromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Object(map) => map
                .into_iter()
                .map(|(key, value)| match T::from_rimu_spanned(value) {
                    Ok(spanned) => Ok((key, spanned.into_inner())),
                    Err(error) => Err(FromRimuError::ObjectValue {
                        key,
                        error: Box::new(error),
                    }),
                })
                .collect(),
            other => Err(wrong_type("an object", other)),
        }
    }
}
