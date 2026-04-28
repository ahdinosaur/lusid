//! Parameter schemas and values for lusid plans.
//!
//! Every plan and every core module declares a `params` schema. This crate defines
//! the three pieces of that system:
//!
//! - **[`ParamType`] / [`ParamField`] / [`ParamTypes`]**: the schema — what shape
//!   of value is accepted (struct or union-of-structs, each with typed fields).
//! - **[`ParamValue`] / [`ParamValues`]**: the parsed, typed value side.
//! - **[`validate`]**: type-checks a Rimu value object against a schema.
//!
//! # Spans are load-bearing
//!
//! Schemas, values, and errors are all `Spanned<T>`. That's how diagnostics point
//! back at the offending line in the user's `.lusid` file. When adding a new type
//! or error variant, keep the span all the way through.
//!
//! # Path-type conventions (see also AGENTS.md)
//!
//! - [`ParamType::HostPath`]: a path on the local machine, normally produced by
//!   Rimu's `host_path("./rel")` stdlib function (which resolves relative to
//!   the source file's directory at construction time). For back-compat,
//!   plain relative strings are still accepted and resolved here against
//!   [`rimu::Span::source`].
//! - [`ParamType::TargetPath`]: an absolute path on the managed machine,
//!   normally produced by Rimu's `target_path("/abs")` stdlib function.
//!   Plain absolute strings are still accepted for back-compat.
//!
//! # Union semantics
//!
//! A [`ParamTypes::Union`] is a list of struct cases. Validation is **first-match**:
//! cases are tried in declaration order, and the first one that validates wins —
//! so authors should order from most-specific to most-general.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use displaydoc::Display;
use indexmap::IndexMap;
use rimu::{
    Number, SerdeValue, SerdeValueError, Span, Spanned, Value, ValueObject, from_serde_value,
};
use rimu_interop::{FromRimu, ToRimuError};
use secrecy::{ExposeSecret, SecretBox};
use serde::de::DeserializeOwned;
use thiserror::Error;

/// A secret plaintext string. Wrapped in [`Arc`] so `ParamValue::Clone` stays
/// cheap, and in [`SecretBox<String>`] so `Debug` is redacted and the plaintext
/// is zeroised when the last clone drops.
///
/// `SecretBox<String>` (rather than `secrecy::SecretString`, a.k.a. `SecretBox<str>`)
/// is used because only the sized form implements `serde::Deserialize`, which is
/// needed so resource param structs can deserialise a secret field.
///
/// Note(cc): the [`Secret`] wrapper protects the plaintext at the boundaries,
/// but the plan evaluator has to hand secrets to Rimu as plain [`Value::String`]
/// (see [`ParamValue::into_rimu`] below, and `secrets_value` in `plan/src/eval.rs`).
/// Rimu is a general-purpose expression language with no notion of secrecy, so
/// intermediate copies made by `+` concatenation, function calls, object/list
/// construction etc. live as ordinary [`String`]s that are not zeroised. agenix
/// / sops-nix sidestep this by materialising secrets at activation time and
/// passing filenames through the evaluator instead of values. Until/unless Rimu
/// grows a `Value::Secret`, this round-trip is an accepted limitation.
pub type Secret = Arc<SecretBox<String>>;

/// Schema node: the allowed shape of a single value.
///
/// - `Literal` matches an exact Rimu value (used to discriminate union cases on a
///   specific `type: "foo"` field).
/// - `List` / `Object` are homogeneous containers — every element/value matches
///   the inner type.
/// - `HostPath` / `TargetPath` accept Rimu's first-class typed paths
///   ([`Value::HostPath`] / [`Value::TargetPath`]) or — for back-compat — a
///   plain [`Value::String`] that satisfies the relative/absolute requirement
///   (see module docs).
/// - `Secret` is `String` at the Rimu level, but materialises into a
///   [`ParamValue::Secret`] so downstream code holds a redacted-on-Debug
///   [`SecretString`] rather than plaintext.
#[derive(Debug, Clone)]
pub enum ParamType {
    Literal(Value),
    Boolean,
    String,
    Number,
    List { item: Box<Spanned<ParamType>> },
    Object { value: Box<Spanned<ParamType>> },
    HostPath,
    TargetPath,
    Secret,
}

#[derive(Debug, Clone)]
pub struct ParamField {
    typ: ParamType,
    optional: bool,
}

impl ParamField {
    pub const fn new(typ: ParamType) -> Self {
        Self {
            typ,
            optional: false,
        }
    }

    pub fn with_optional(self) -> Self {
        Self {
            typ: self.typ,
            optional: true,
        }
    }

    pub fn typ(&self) -> &ParamType {
        &self.typ
    }

    pub fn optional(&self) -> &bool {
        &self.optional
    }
}

/// Ordered map of field name → field schema. `IndexMap` is deliberate — we preserve
/// declaration order for stable diagnostics and rendering.
pub type ParamsStruct = IndexMap<String, Spanned<ParamField>>;

/// Top-level schema: either a single struct, or a union of candidate structs.
#[derive(Debug, Clone)]
pub enum ParamTypes {
    /// A single object structure: keys -> fields
    Struct(ParamsStruct),
    /// A union of possible object structures. Validation tries cases in order
    /// and returns the first that matches (see module docs).
    Union(Vec<ParamsStruct>),
}

/// A parameter value after type-directed conversion.
///
/// Mirrors [`ParamType`] variants but holds a concrete value. `HostPath`
/// holds a [`PathBuf`] resolved by Rimu's `host_path(...)` against the source
/// file's directory (or by the back-compat plain-string branch in
/// [`Self::from_rimu_spanned`]) — downstream consumers never need to know
/// where the source file lived.
///
/// `Secret` wraps its plaintext in a [`Secret`] (an `Arc<SecretBox<String>>`) so
/// clones are cheap and `Debug` stays redacted.
#[derive(Debug, Clone)]
pub enum ParamValue {
    Literal(Value),
    Boolean(bool),
    String(String),
    Number(Number),
    List(Vec<Spanned<ParamValue>>),
    Object(IndexMap<String, Spanned<ParamValue>>),
    HostPath(PathBuf),
    TargetPath(String),
    Secret(Secret),
}

impl ParamValue {
    pub fn into_rimu_spanned(value: Spanned<Self>) -> Spanned<Value> {
        let (value, span) = value.take();
        Spanned::new(value.into_rimu(), span)
    }

    pub fn into_rimu(self) -> Value {
        match self {
            ParamValue::Literal(value) => value,
            ParamValue::Boolean(value) => Value::Boolean(value),
            ParamValue::String(value) => Value::String(value),
            ParamValue::Number(number) => Value::Number(number),
            ParamValue::List(items) => {
                let items = items.into_iter().map(Self::into_rimu_spanned).collect();
                Value::List(items)
            }
            ParamValue::Object(map) => {
                let map = map
                    .into_iter()
                    .map(|(key, value)| (key, Self::into_rimu_spanned(value)))
                    .collect();
                Value::Object(map)
            }
            // Now that Rimu has first-class `HostPath` / `TargetPath`, produce
            // the typed forms — downstream consumers (nested plan params,
            // resources implementing `FromRimu`) keep the path-vs-string
            // distinction end-to-end instead of round-tripping through a bare
            // `Value::String`.
            ParamValue::HostPath(path) => Value::HostPath(path),
            ParamValue::TargetPath(path) => Value::TargetPath(path),
            ParamValue::Secret(secret) => Value::String(secret.expose_secret().to_owned()),
        }
    }
}

#[derive(Debug, Clone, Error, Display)]
pub enum ParamValueFromRimuError {
    /// Expected literal value ({value}) to equal type ({typ})
    LiteralNotEqual { value: Box<Value>, typ: Box<Value> },

    /// Error with list at index {index}: {error}
    List {
        index: usize,
        error: Box<Spanned<ParamValueFromRimuError>>,
    },

    /// Error with object at key {key}: {error}
    Object {
        key: String,
        error: Box<Spanned<ParamValueFromRimuError>>,
    },

    /// Host path source needs parent: {source_path}
    HostPathSourceNeedsParent { source_path: PathBuf },

    /// Unexpected param type + value case
    UnexpectedParamTypeValueCase {
        typ: Box<ParamType>,
        value: Box<Value>,
    },
}

impl ParamValue {
    fn from_rimu_spanned(
        value: Spanned<Value>,
        typ: ParamType,
    ) -> Result<Spanned<Self>, Spanned<ParamValueFromRimuError>> {
        let (value, span) = value.take();

        let result = match (typ, value) {
            (ParamType::Literal(typ), value) => {
                if typ != value {
                    Err(ParamValueFromRimuError::LiteralNotEqual {
                        value: Box::new(value),
                        typ: Box::new(typ),
                    })
                } else {
                    Ok(ParamValue::Literal(value))
                }
            }
            (ParamType::Boolean, Value::Boolean(value)) => Ok(ParamValue::Boolean(value)),
            (ParamType::String, Value::String(value)) => Ok(ParamValue::String(value)),
            (ParamType::Number, Value::Number(value)) => Ok(ParamValue::Number(value)),
            (ParamType::List { item: item_type }, Value::List(items)) => {
                let items = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        ParamValue::from_rimu_spanned(item, item_type.inner().clone()).map_err(
                            |error| {
                                Spanned::new(
                                    ParamValueFromRimuError::List {
                                        index,
                                        error: Box::new(error),
                                    },
                                    span.clone(),
                                )
                            },
                        )
                    })
                    .collect::<Result<_, _>>()?;
                Ok(ParamValue::List(items))
            }
            (ParamType::Object { value: value_type }, Value::Object(object)) => {
                let object = object
                    .into_iter()
                    .map(|(key, value)| {
                        Ok((
                            key.clone(),
                            ParamValue::from_rimu_spanned(value, value_type.inner().clone())
                                .map_err(|error| {
                                    Spanned::new(
                                        ParamValueFromRimuError::Object {
                                            key,
                                            error: Box::new(error),
                                        },
                                        span.clone(),
                                    )
                                })?,
                        ))
                    })
                    .collect::<Result<_, _>>()?;
                Ok(ParamValue::Object(object))
            }
            // Rimu's `host_path("./rel")` resolves the relative input against
            // the source-file directory at construction time, so the [`PathBuf`]
            // here is already in the form downstream consumers want — pass it
            // through unchanged.
            (ParamType::HostPath, Value::HostPath(path)) => Ok(ParamValue::HostPath(path)),

            // Plain-string back-compat path. Resolve the relative input against
            // the source file's directory the same way Rimu's `host_path` would
            // — without a parent directory we'd have nowhere to resolve against.
            (ParamType::HostPath, Value::String(value)) => {
                let value_path = PathBuf::from(value);
                let source_path = PathBuf::from(span.source().as_str());
                let source_dir_path = source_path.parent();
                if let Some(source_dir_path) = source_dir_path {
                    let host_path = source_dir_path.join(value_path);
                    Ok(ParamValue::HostPath(host_path))
                } else {
                    Err(ParamValueFromRimuError::HostPathSourceNeedsParent { source_path })
                }
            }
            (ParamType::TargetPath, Value::TargetPath(path)) => Ok(ParamValue::TargetPath(path)),
            (ParamType::TargetPath, Value::String(value)) => Ok(ParamValue::TargetPath(value)),
            (ParamType::Secret, Value::String(value)) => Ok(ParamValue::Secret(Arc::new(
                SecretBox::new(Box::new(value)),
            ))),
            (typ, value) => Err(ParamValueFromRimuError::UnexpectedParamTypeValueCase {
                typ: Box::new(typ),
                value: Box::new(value),
            }),
        };

        result
            .map(|value| Spanned::new(value, span.clone()))
            .map_err(|error| Spanned::new(error, span.clone()))
    }
}

/// Map of parameter name → typed value. Wraps [`IndexMap`] to preserve declaration
/// order for stable rendering and iteration.
#[derive(Debug, Clone, Default)]
pub struct ParamValues(IndexMap<String, Spanned<ParamValue>>);

#[derive(Debug, Clone, Error, Display)]
pub enum ParamValuesFromTypeError {
    /// Failed to convert serializable value to Rimu: {0}
    ToRimu(#[from] ToRimuError),

    /// Failed to convert Rimu value into parameter values: {0}
    FromRimu(#[from] ParamValuesFromRimuError),

    /// Failed validation: {0}
    Validation(#[from] ParamsValidationError),
}

#[derive(Debug, Clone, Error, Display)]
pub enum ParamValuesFromRimuError {
    /// Expected an object mapping parameter names to values
    NotAnObject,

    /// Expected param missing: {key}
    ParamMissing { key: String },

    /// Error with param {key}: {error}
    Param {
        key: String,
        error: Box<Spanned<ParamValueFromRimuError>>,
    },
}

impl ParamValues {
    pub fn from_rimu_spanned(
        value: Spanned<Value>,
        type_struct: ParamsStruct,
    ) -> Result<Spanned<Self>, Spanned<ParamValuesFromRimuError>> {
        let (value, span) = value.take();

        let Value::Object(object_value) = value else {
            return Err(Spanned::new(ParamValuesFromRimuError::NotAnObject, span));
        };

        let mut param_values = IndexMap::new();

        for (key, field_value) in object_value.into_iter() {
            let field_type = type_struct
                .get(&key)
                .ok_or_else(|| {
                    Spanned::new(
                        ParamValuesFromRimuError::ParamMissing { key: key.clone() },
                        span.clone(),
                    )
                })?
                .inner();

            if *field_type.optional() && matches!(field_value.inner(), Value::Null) {
                continue;
            }

            let param_value = ParamValue::from_rimu_spanned(field_value, field_type.typ().clone())
                .map_err(|error| {
                    Spanned::new(
                        ParamValuesFromRimuError::Param {
                            key: key.clone(),
                            error: Box::new(error),
                        },
                        span.clone(),
                    )
                })?;
            param_values.insert(key, param_value);
        }

        Ok(Spanned::new(ParamValues(param_values), span))
    }

    pub fn into_rimu_spanned(value: Spanned<Self>) -> Spanned<Value> {
        let (value, span) = value.take();
        Spanned::new(value.into_rimu(), span)
    }

    pub fn into_rimu(self) -> Value {
        let object = self
            .0
            .into_iter()
            .map(|(key, value)| (key, ParamValue::into_rimu_spanned(value)))
            .collect();
        Value::Object(object)
    }

    pub fn get(&self, key: &str) -> Option<&Spanned<ParamValue>> {
        self.0.get(key)
    }

    pub fn into_type<T>(self) -> Result<T, SerdeValueError>
    where
        T: DeserializeOwned,
    {
        let value = self.into_rimu();
        let serde_value = SerdeValue::from(value);
        from_serde_value(serde_value)
    }
}

#[derive(Debug, Clone, Error, Display)]
pub enum ParamTypeFromRimuError {
    /// Expected an object for parameter type
    NotAnObject,
    /// Missing property: "type"
    HasNoType,
    /// The "type" property must be a string
    TypeNotAString { span: Span },
    /// Unknown parameter type: {0}
    UnknownType(String),
    /// List type is missing required "item" property
    ListMissingItem,
    /// Invalid "item" type in list: {0:?}
    ListItem(Box<Spanned<ParamTypeFromRimuError>>),
    /// Object type is missing required "value" property
    ObjectMissingValue,
    /// Invalid "value" type in object: {0:?}
    ObjectValue(Box<Spanned<ParamTypeFromRimuError>>),
}

impl FromRimu for ParamType {
    type Error = ParamTypeFromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        let Value::Object(mut object) = value else {
            return Err(ParamTypeFromRimuError::NotAnObject);
        };

        let Some(typ) = object.get("type") else {
            return Err(ParamTypeFromRimuError::HasNoType);
        };

        let (typ, typ_span) = typ.clone().take();
        let Value::String(typ) = typ else {
            return Err(ParamTypeFromRimuError::TypeNotAString { span: typ_span });
        };

        match typ.as_str() {
            "boolean" => Ok(ParamType::Boolean),
            "string" => Ok(ParamType::String),
            "number" => Ok(ParamType::Number),
            "host-path" => Ok(ParamType::HostPath),
            "target-path" => Ok(ParamType::TargetPath),
            "secret" => Ok(ParamType::Secret),
            "list" => {
                let item = object
                    .swap_remove("item")
                    .ok_or(ParamTypeFromRimuError::ListMissingItem)?;
                let item = ParamType::from_rimu_spanned(item)
                    .map_err(|error| ParamTypeFromRimuError::ListItem(Box::new(error)))?;
                Ok(ParamType::List {
                    item: Box::new(item),
                })
            }
            "object" => {
                let value = object
                    .swap_remove("value")
                    .ok_or(ParamTypeFromRimuError::ObjectMissingValue)?;
                let value = ParamType::from_rimu_spanned(value)
                    .map_err(|error| ParamTypeFromRimuError::ObjectValue(Box::new(error)))?;
                Ok(ParamType::Object {
                    value: Box::new(value),
                })
            }
            other => Err(ParamTypeFromRimuError::UnknownType(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Error, Display)]
pub enum ParamFieldFromRimuError {
    /// Expected an object for parameter field
    NotAnObject,
    /// The "optional" property must be a boolean
    OptionalNotABoolean { span: Span },
    /// Invalid field type: {0:?}
    FieldType(#[from] ParamTypeFromRimuError),
}

impl FromRimu for ParamField {
    type Error = ParamFieldFromRimuError;

    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        let Value::Object(mut object) = value else {
            return Err(ParamFieldFromRimuError::NotAnObject);
        };

        let optional = if let Some(optional_value) = object.swap_remove("optional") {
            let (inner, span) = optional_value.take();
            match inner {
                Value::Boolean(b) => b,
                _ => {
                    return Err(ParamFieldFromRimuError::OptionalNotABoolean { span });
                }
            }
        } else {
            false
        };

        let typ = ParamType::from_rimu(Value::Object(object))?;
        Ok(ParamField { typ, optional })
    }
}

#[derive(Debug, Clone, Error, Display)]
pub enum ParamTypesFromRimuError {
    /// Expected an object (struct) or a list (union) for parameter types
    NotAnObjectOrList,
    /// Invalid struct entry for key "{key}": {error:?}
    StructEntry {
        key: String,
        error: Box<Spanned<ParamFieldFromRimuError>>,
    },
    /// Union item at index {index} is not an object
    UnionItemNotAnObject { index: usize, span: Span },
    /// Invalid union item entry for key "{key}" at index {index}: {error:?}
    UnionItemEntry {
        index: usize,
        key: String,
        error: Box<Spanned<ParamFieldFromRimuError>>,
    },
}

impl FromRimu for ParamTypes {
    type Error = ParamTypesFromRimuError;

    /// Parse a schema declaration in the plan:
    /// - An **object** defines a [`ParamTypes::Struct`] — the map of fields.
    /// - A **list** defines a [`ParamTypes::Union`] — each item is an object
    ///   defining one candidate case (first-match wins during validation).
    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Object(map) => {
                let mut out: IndexMap<String, Spanned<ParamField>> =
                    IndexMap::with_capacity(map.len());

                for (key, value) in map {
                    let field = match ParamField::from_rimu_spanned(value) {
                        Ok(field) => field,
                        Err(error) => {
                            return Err(ParamTypesFromRimuError::StructEntry {
                                key: key.clone(),
                                error: Box::new(error),
                            });
                        }
                    };
                    out.insert(key, field);
                }

                Ok(ParamTypes::Struct(out))
            }
            Value::List(items) => {
                let mut cases: Vec<IndexMap<String, Spanned<ParamField>>> =
                    Vec::with_capacity(items.len());

                for (index, spanned_item) in items.into_iter().enumerate() {
                    let (inner, span) = spanned_item.clone().take();
                    let Value::Object(case_map) = inner else {
                        return Err(ParamTypesFromRimuError::UnionItemNotAnObject { index, span });
                    };

                    let mut case_out: IndexMap<String, Spanned<ParamField>> =
                        IndexMap::with_capacity(case_map.len());

                    for (key, value) in case_map {
                        let field = match ParamField::from_rimu_spanned(value) {
                            Ok(field) => field,
                            Err(error) => {
                                return Err(ParamTypesFromRimuError::UnionItemEntry {
                                    index,
                                    key: key.clone(),
                                    error: Box::new(error),
                                });
                            }
                        };
                        case_out.insert(key, field);
                    }

                    cases.push(case_out);
                }

                Ok(ParamTypes::Union(cases))
            }
            _ => Err(ParamTypesFromRimuError::NotAnObjectOrList),
        }
    }
}

#[derive(Debug, Clone, Error, Display)]
pub enum ValidateValueError {
    /// Value does not match expected type
    TypeMismatch {
        expected_type: Box<Spanned<ParamType>>,
        got_value: Box<Spanned<Value>>,
    },

    /// Invalid list item at index {index}: {error:?}
    ListItem {
        index: usize,
        #[source]
        error: Box<ValidateValueError>,
    },

    /// Invalid object entry for key "{key}": {error:?}
    ObjectEntry {
        key: String,
        #[source]
        error: Box<ValidateValueError>,
    },

    /// Expected a secret string, got Null — check that `ctx.secrets.<name>` matches an existing `<name>.age` file
    //
    // Note(cc): This is a partial mitigation for typos in `ctx.secrets.<name>`
    // references. It only fires when the Null flows directly into a
    // `ParamType::Secret` field. A Null that gets used in e.g. string
    // concatenation or passed to a non-Secret param won't be caught here —
    // see the `Note(cc)` in `plan/src/eval.rs` for the full fix (strict
    // `ctx.secrets` map that errors at access time, which needs Rimu support).
    NullSecret {
        expected_type: Box<Spanned<ParamType>>,
    },
}

#[derive(Debug, Clone, Error, Display)]
pub enum ParamValidationError {
    /// Missing required parameter "{key}"
    MissingParam {
        key: String,
        expected_type: Box<Spanned<ParamType>>,
    },
    /// Unknown parameter "{key}"
    UnknownParam {
        key: String,
        value: Box<Spanned<Value>>,
    },
    /// Invalid parameter "{key}": {error:?}
    InvalidParam {
        key: String,
        error: Box<ValidateValueError>,
    },
}

#[derive(Debug, Clone, Error, Display)]
#[displaydoc("Parameters struct did not match all fields")]
pub struct ParamsStructValidationError {
    errors: Vec<ParamValidationError>,
}

#[derive(Debug, Clone, Error, Display)]
pub enum ParamsValidationError {
    /// Parameter values without parameter types
    ValuesWithoutTypes,
    /// Parameter types without parameter values
    TypesWithoutValues,
    /// Expected an object for parameter values
    ValuesNotAnObject,
    /// Parameter struct did not match all fields: {0}
    Struct(#[from] Box<ParamsStructValidationError>),
    /// Parameter union did not match any case: {case_errors:?}
    Union {
        case_errors: Vec<ParamsStructValidationError>,
    },
    /// Parameter union type is empty
    EmptyUnion,
}

fn mismatch(typ: &Spanned<ParamType>, value: &Spanned<Value>) -> ValidateValueError {
    ValidateValueError::TypeMismatch {
        expected_type: Box::new(typ.clone()),
        got_value: Box::new(value.clone()),
    }
}

fn validate_type(
    param_type: &Spanned<ParamType>,
    value: &Spanned<Value>,
) -> Result<(), ValidateValueError> {
    let typ_inner = param_type.inner();
    let value_inner = value.inner();

    match typ_inner {
        ParamType::Literal(literal) => {
            if value.inner() == literal {
                Ok(())
            } else {
                Err(mismatch(param_type, value))
            }
        }
        ParamType::Boolean => match value_inner {
            Value::Boolean(_) => Ok(()),
            _ => Err(mismatch(param_type, value)),
        },

        ParamType::String => match value_inner {
            Value::String(_) => Ok(()),
            _ => Err(mismatch(param_type, value)),
        },

        ParamType::Number => match value_inner {
            Value::Number(_) => Ok(()),
            _ => Err(mismatch(param_type, value)),
        },

        // Accept Rimu's first-class `Value::HostPath` directly. Plain
        // strings are still allowed for back-compat with plans (and host-side
        // helpers like `to_rimu` for `User::home`) that haven't migrated to
        // the `host_path("./rel")` stdlib constructor; relative-path enforcement
        // matches the pre-typed-path behavior.
        ParamType::HostPath => match value_inner {
            Value::HostPath(_) => Ok(()),
            Value::String(path) if Path::new(path).is_relative() => Ok(()),
            _ => Err(mismatch(param_type, value)),
        },

        // Same shape as `HostPath`: accept the typed form, fall through to a
        // plain (absolute) string for back-compat.
        ParamType::TargetPath => match value_inner {
            Value::TargetPath(_) => Ok(()),
            Value::String(path) if Path::new(path).is_absolute() => Ok(()),
            _ => Err(mismatch(param_type, value)),
        },

        ParamType::Secret => match value_inner {
            // Note(cc): empty-string secrets pass — a secret file can legitimately
            // contain an empty plaintext (rare, but not impossible). If we ever want
            // to reject this, do it here with a dedicated error variant rather than
            // reusing `NullSecret` (which speaks specifically about typo-on-lookup).
            Value::String(_) => Ok(()),
            Value::Null => Err(ValidateValueError::NullSecret {
                expected_type: Box::new(param_type.clone()),
            }),
            _ => Err(mismatch(param_type, value)),
        },

        ParamType::List { item } => {
            let Value::List(items) = value_inner else {
                return Err(mismatch(param_type, value));
            };

            for (index, item_value) in items.iter().enumerate() {
                if let Err(error) = validate_type(item, item_value) {
                    return Err(ValidateValueError::ListItem {
                        index,
                        error: Box::new(error),
                    });
                }
            }

            Ok(())
        }

        ParamType::Object { value: value_type } => {
            let Value::Object(map) = value_inner else {
                return Err(mismatch(param_type, value));
            };

            for (key, entry_value) in map.iter() {
                if let Err(error) = validate_type(value_type, entry_value) {
                    return Err(ValidateValueError::ObjectEntry {
                        key: key.clone(),
                        error: Box::new(error),
                    });
                }
            }

            Ok(())
        }
    }
}

fn validate_struct(
    fields: &IndexMap<String, Spanned<ParamField>>,
    values: &ValueObject,
) -> Result<(), ParamsStructValidationError> {
    let mut errors: Vec<ParamValidationError> = Vec::new();

    // Requiredness and per-field validation.
    for (key, spanned_field) in fields.iter() {
        let (field, field_span) = spanned_field.clone().take();
        let spanned_type = Spanned::new(field.typ().clone(), field_span);

        match values.get(key) {
            Some(spanned_value) => {
                if let Err(error) = validate_type(&spanned_type, spanned_value) {
                    errors.push(ParamValidationError::InvalidParam {
                        key: key.clone(),
                        error: Box::new(error),
                    });
                }
            }
            None => {
                if !field.optional {
                    errors.push(ParamValidationError::MissingParam {
                        key: key.clone(),
                        expected_type: Box::new(spanned_type),
                    });
                }
            }
        }
    }

    // Unknown keys.
    for (key, spanned_value) in values.iter() {
        if !fields.contains_key(key) {
            errors.push(ParamValidationError::UnknownParam {
                key: key.clone(),
                value: Box::new(spanned_value.clone()),
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ParamsStructValidationError { errors })
    }
}

/// Validate parameter values against a schema.
///
/// - `Struct` schemas must match all fields exactly (required fields present,
///   unknown fields rejected, each value the right type).
/// - `Union` schemas succeed on the first case that validates; if none match,
///   all per-case errors are returned together so users can see why each case
///   failed.
///
/// Returns the matching [`ParamsStruct`] on success (useful to downstream code
/// that needs to know which union case won), or `None` when both types and
/// values are absent (a valid "parameterless" plan).
pub fn validate(
    param_types: Option<&Spanned<ParamTypes>>,
    param_values: Option<&Spanned<Value>>,
) -> Result<Option<ParamsStruct>, ParamsValidationError> {
    let (param_types, param_values) = match (param_types, param_values) {
        (Some(param_types), Some(param_values)) => (param_types, param_values),
        (Some(_), None) => {
            return Err(ParamsValidationError::TypesWithoutValues);
        }
        (None, Some(_)) => {
            return Err(ParamsValidationError::ValuesWithoutTypes);
        }
        (None, None) => {
            return Ok(None);
        }
    };

    let param_types = param_types.inner();
    let param_values = param_values.inner();

    let Value::Object(param_values) = param_values else {
        return Err(ParamsValidationError::ValuesNotAnObject);
    };

    match param_types {
        ParamTypes::Struct(map) => {
            validate_struct(map, param_values).map_err(Box::new)?;

            Ok(Some(map.clone()))
        }
        ParamTypes::Union(cases) => {
            if cases.is_empty() {
                return Err(ParamsValidationError::EmptyUnion);
            }

            let mut case_errors: Vec<ParamsStructValidationError> = Vec::with_capacity(cases.len());

            for case in cases {
                match validate_struct(case, param_values) {
                    Ok(()) => return Ok(Some(case.clone())),
                    Err(error) => case_errors.push(error),
                }
            }

            Err(ParamsValidationError::Union { case_errors })
        }
    }
}
