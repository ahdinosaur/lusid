//! Integration tests for `#[derive(FromRimu)]`.

use rimu::{Number, SourceId, Span, Spanned, Value};
use rimu_interop::{FromRimu, FromRimuError};

fn span() -> Span {
    Span::new(SourceId::empty(), 0, 0)
}

fn s<T: Clone>(value: T) -> Spanned<T> {
    Spanned::new(value, span())
}

fn obj<const N: usize>(entries: [(&str, Value); N]) -> Value {
    let mut map = indexmap::IndexMap::new();
    for (key, value) in entries {
        map.insert(key.to_string(), s(value));
    }
    Value::Object(map)
}

// ---------------------------------------------------------------------------
// Plain struct
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRimu, PartialEq)]
struct Plain {
    name: String,
    count: u32,
    enabled: Option<bool>,
}

#[test]
fn plain_struct_reads_required_and_optional_fields() {
    let value = obj([
        ("name", Value::String("hello".into())),
        ("count", Value::Number(Number::from(42_u32))),
        ("enabled", Value::Boolean(true)),
    ]);
    let parsed = Plain::from_rimu(value).unwrap();
    assert_eq!(
        parsed,
        Plain {
            name: "hello".into(),
            count: 42,
            enabled: Some(true),
        }
    );
}

#[test]
fn plain_struct_optional_field_missing_is_none() {
    let value = obj([
        ("name", Value::String("hi".into())),
        ("count", Value::Number(Number::from(1_u32))),
    ]);
    let parsed = Plain::from_rimu(value).unwrap();
    assert_eq!(parsed.enabled, None);
}

#[test]
fn plain_struct_optional_field_null_is_none() {
    let value = obj([
        ("name", Value::String("hi".into())),
        ("count", Value::Number(Number::from(1_u32))),
        ("enabled", Value::Null),
    ]);
    let parsed = Plain::from_rimu(value).unwrap();
    assert_eq!(parsed.enabled, None);
}

#[test]
fn plain_struct_missing_required_errors() {
    let value = obj([("name", Value::String("hi".into()))]);
    let err = Plain::from_rimu(value).unwrap_err();
    assert!(matches!(err, FromRimuError::MissingField { name: "count" }));
}

#[test]
fn plain_struct_unknown_field_errors() {
    let value = obj([
        ("name", Value::String("hi".into())),
        ("count", Value::Number(Number::from(1_u32))),
        ("extra", Value::Boolean(false)),
    ]);
    let err = Plain::from_rimu(value).unwrap_err();
    match err {
        FromRimuError::UnknownField { name, .. } => assert_eq!(name, "extra"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn plain_struct_wrong_type_errors() {
    let err = Plain::from_rimu(Value::Boolean(true)).unwrap_err();
    assert!(matches!(err, FromRimuError::WrongType { .. }));
}

#[test]
fn plain_struct_field_type_error_is_wrapped() {
    let value = obj([
        ("name", Value::Boolean(true)),
        ("count", Value::Number(Number::from(1_u32))),
    ]);
    let err = Plain::from_rimu(value).unwrap_err();
    match err {
        FromRimuError::Field { name, .. } => assert_eq!(name, "name"),
        other => panic!("unexpected: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Tagged enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRimu, PartialEq)]
#[rimu(tag = "state", rename_all = "kebab-case")]
enum Tagged {
    Sourced { source: String, target: String },
    Absent { target: String },
}

#[test]
fn tagged_enum_picks_variant_by_discriminant() {
    let value = obj([
        ("state", Value::String("sourced".into())),
        ("source", Value::String("./src".into())),
        ("target", Value::String("/dst".into())),
    ]);
    let parsed = Tagged::from_rimu(value).unwrap();
    assert_eq!(
        parsed,
        Tagged::Sourced {
            source: "./src".into(),
            target: "/dst".into(),
        }
    );
}

#[test]
fn tagged_enum_kebab_case_normalises_variant_ident() {
    // Variant `Absent` matches discriminant string "absent".
    let value = obj([
        ("state", Value::String("absent".into())),
        ("target", Value::String("/dst".into())),
    ]);
    let parsed = Tagged::from_rimu(value).unwrap();
    assert_eq!(
        parsed,
        Tagged::Absent {
            target: "/dst".into()
        }
    );
}

#[test]
fn tagged_enum_unknown_variant_errors() {
    let value = obj([("state", Value::String("nope".into()))]);
    let err = Tagged::from_rimu(value).unwrap_err();
    match err {
        FromRimuError::UnknownVariant { tag, value, .. } => {
            assert_eq!(tag, "state");
            assert_eq!(value, "nope");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn tagged_enum_missing_discriminant_errors() {
    let value = obj([("source", Value::String("./src".into()))]);
    let err = Tagged::from_rimu(value).unwrap_err();
    assert!(matches!(
        err,
        FromRimuError::MissingDiscriminant { tag: "state" }
    ));
}

// ---------------------------------------------------------------------------
// Untagged enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRimu, PartialEq)]
#[rimu(untagged)]
enum Untagged {
    One { single: String },
    Many { items: Vec<String> },
}

#[test]
fn untagged_enum_first_match_wins() {
    let single = obj([("single", Value::String("only".into()))]);
    let parsed = Untagged::from_rimu(single).unwrap();
    assert_eq!(
        parsed,
        Untagged::One {
            single: "only".into()
        }
    );
}

#[test]
fn untagged_enum_falls_through_to_second() {
    let many = obj([(
        "items",
        Value::List(vec![
            s(Value::String("a".into())),
            s(Value::String("b".into())),
        ]),
    )]);
    let parsed = Untagged::from_rimu(many).unwrap();
    assert_eq!(
        parsed,
        Untagged::Many {
            items: vec!["a".into(), "b".into()]
        }
    );
}

#[test]
fn untagged_enum_no_match_returns_all_errors() {
    let value = obj([("nope", Value::String("oops".into()))]);
    let err = Untagged::from_rimu(value).unwrap_err();
    match err {
        FromRimuError::NoVariantMatched { case_errors } => {
            assert_eq!(case_errors.len(), 2);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Field rename
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRimu, PartialEq)]
struct Renamed {
    #[rimu(rename = "src")]
    source: String,
}

#[test]
fn field_rename_changes_lookup_key() {
    let value = obj([("src", Value::String("hello".into()))]);
    let parsed = Renamed::from_rimu(value).unwrap();
    assert_eq!(
        parsed,
        Renamed {
            source: "hello".into()
        }
    );
}

// ---------------------------------------------------------------------------
// Variant-level rename
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRimu, PartialEq)]
#[rimu(tag = "status")]
enum VariantRenamed {
    #[rimu(rename = "install")]
    Install { command: String },
    #[rimu(rename = "uninstall")]
    Uninstall { command: String },
}

#[test]
fn variant_level_rename_overrides_lookup() {
    let value = obj([
        ("status", Value::String("install".into())),
        ("command", Value::String("apt install foo".into())),
    ]);
    let parsed = VariantRenamed::from_rimu(value).unwrap();
    assert_eq!(
        parsed,
        VariantRenamed::Install {
            command: "apt install foo".into()
        }
    );
}

// ---------------------------------------------------------------------------
// Span propagation through untagged variants
// ---------------------------------------------------------------------------

fn fake_source() -> SourceId {
    use std::str::FromStr;
    SourceId::from_str("file:///plan.lusid").unwrap_or_else(|_| SourceId::empty())
}

#[test]
fn untagged_enum_case_errors_carry_real_outer_span() {
    let real_span = Span::new(fake_source(), 10, 20);
    let value = Value::String("not-an-object".into());
    let spanned = Spanned::new(value, real_span.clone());

    let err = Untagged::from_rimu_spanned(spanned).unwrap_err();
    let (inner, outer_span) = err.take();
    // Outer span must equal the input's real span.
    assert_eq!(outer_span, real_span);

    // Each per-variant error must also carry the real span (not the synthetic
    // empty-source one used by the bare from_rimu path).
    match inner {
        FromRimuError::NoVariantMatched { case_errors } => {
            assert!(!case_errors.is_empty());
            for case_error in &case_errors {
                assert_eq!(case_error.span(), real_span);
            }
        }
        other => panic!("unexpected: {other:?}"),
    }
}
