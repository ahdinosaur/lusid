# TODO — params system: three options

Once Rimu lands `Value::HostPath` / `Value::TargetPath` directly (see
`../rimu/TODO.md`), lusid no longer has to fabricate the type discipline
itself — Rimu's evaluator carries it. That changes the calculus for how
much of lusid's params plumbing is still pulling its weight.

This file lays out three options for what lusid does next, ordered from
"smallest change" to "deepest refactor". Pick one before starting work.

## Background: what the current pipeline does

A plan declares a `params` schema and is called with a `params` object.
The flow is:

```
Rimu Value           (params object from the caller)
   │
   ▼  validate(schema, value)              ── checks shape only
   │
   ▼  ParamValue::from_rimu_spanned(...)   ── typed conversion: relative
   │                                          string → resolved PathBuf
   │                                          for HostPath, etc.
ParamValues          (typed: HostPath(PathBuf), TargetPath(String), ...)
   │
   ▼  into_rimu(PathExposure::Plain)       ── flatten paths to strings
   │
Value (flattened)    (Value::String everywhere)
   │
   ▼  Into::into                           ── drop spans
   │
SerdeValue
   │
   ▼  T::deserialize                       ── via #[derive(Deserialize)]
   │
ConfigParams         (resource's typed Rust struct)
```

Two things to notice:

1. **`ParamValue` is a typed mirror of `Value`.** It exists so lusid can
   resolve relative paths once (against the plan's source dir) and carry
   the result around in a typed shape during validation, before flattening
   for serde.
2. **Path-type information is lost at the serde boundary.** `HostPath`
   and `TargetPath` both flatten to `Value::String` because serde's data
   model has no way to express "this string is a HostPath." The resource
   gets a `PathBuf` or `String` field and can't tell which it came from.

Once Rimu provides `Value::HostPath` / `Value::TargetPath` natively,
`ParamValue::HostPath` and `ParamValue::TargetPath` become redundant
mirrors of the Rimu types. The serde boundary still flattens regardless,
but now the question is: do we still need the lusid-side typed mirror?

---

## Option A — keep the current architecture; just consume the new types

**What changes:** very little. `ParamValue::from_rimu_spanned` learns to
recognise `Value::HostPath` and `Value::TargetPath` directly (instead of
`Value::Tagged { tag: "host-path", ... }`), and `into_rimu` produces them
(instead of tagged envelopes). The `PathExposure` enum and its callers
stay; the `Plain` arm is still required for the serde step.

**Cost:** trivial. A few match arms in `params/src/lib.rs`. Resources
keep their `#[derive(Deserialize)]`.

**Pros**
- Smallest diff. Resources untouched. Tests mostly carry over.
- Keeps the tagged-envelope idea (now baked into Rimu) at arm's length —
  lusid can pretend nothing changed except that paths are now first-class.

**Cons**
- Still flattens paths at the serde boundary — so a resource that wants
  to know whether a field came from `host_path("...")` vs a literal
  string can't.
- `ParamValue` is now a near-duplicate of Rimu's `Value` for path
  variants. Carrying both is dead weight, and a future contributor will
  ask why.
- Errors on the resource side still use serde's "expected string, got
  X" messages, which can't carry Rimu spans.

**When to pick this:** if no current resource needs to discriminate
HostPath-from-string at runtime (true today), and the upcoming roadmap
doesn't push that need either. It's the path of least surprise. Treat it
as the *default* and revisit only if a real call site forces the issue.

---

## Option B — replace `#[derive(Deserialize)]` on resource params with `FromRimu`

**What changes:** every resource's `Params` type stops deriving
`serde::Deserialize` and instead implements `lusid_rimu_interop::FromRimu`
(the trait already used for `ParamType` / `ParamField` / `ParamTypes`).
The `into_type::<T>()` call in `plan/src/core.rs` becomes
`T::from_rimu(value)`. `SerdeValue` disappears from the resource path
entirely. `PathExposure` collapses to a single variant; `into_rimu` no
longer needs the `Plain` shape.

**Cost:** moderate. Each resource is one custom impl. The pain point is
**serde's tagged-enum support**: e.g. `FileParams` is

```rust
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum FileParams { Sourced { ... }, Contents { ... }, ... }
```

`#[derive(Deserialize)]` does the discriminant dispatch automatically.
`FromRimu` would have to do it by hand — read the `state` field,
match on it, then read the rest. Multiply across `FileParams`,
`PodmanParams`, `SystemdParams`, etc.

**Mitigation:** a `#[derive(FromRimu)]` proc-macro in `rimu-interop`
that mirrors the parts of `#[derive(Deserialize)]` we actually use:
named struct fields, `#[serde(tag = "...")]`-style enums, `Option<T>`
for optional fields, newtype wrappers. If we're committing to Option B
across all ~12 resources, the macro pays for itself; without it,
hand-written `FromRimu` impls are tedious but mechanical.

### Sketch

```rust
// resource/src/resources/file.rs
#[derive(Debug, Clone, FromRimu)]
#[rimu(tag = "state", rename_all = "kebab-case")]
pub enum FileParams {
    Sourced {
        source: FilePath,           // newtype around PathBuf
        path: FilePath,
        mode: Option<FileMode>,
        user: Option<FileUser>,
        group: Option<FileGroup>,
    },
    // ...
}

// FilePath itself implements FromRimu — dispatches on Value::HostPath
// vs Value::TargetPath vs Value::String, returns the right inner kind.
impl FromRimu for FilePath {
    type Error = FilePathFromRimuError;
    fn from_rimu(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::HostPath(p) => Ok(FilePath::Host(p)),
            Value::TargetPath(p) => Ok(FilePath::Target(p)),
            Value::String(s)    => Err(FilePathFromRimuError::PlainString { value: s }),
            other               => Err(FilePathFromRimuError::WrongType { got: other }),
        }
    }
}
```

```rust
// plan/src/core.rs
fn core_module_for_resource<R: ResourceType>(
    params_value: Option<Spanned<Value>>,
) -> Result<R::Params, PlanItemToResourceError> {
    let params_value = params_value.ok_or(PlanItemToResourceError::MissingParams)?;
    let param_types = R::param_types();
    validate(param_types.as_ref(), Some(&params_value))?;
    R::Params::from_rimu(params_value.into_inner())
        .map_err(PlanItemToResourceError::ParamsFromRimu)
}
```

**Pros**
- **Path-type info preserved end-to-end.** A resource that wants to
  distinguish HostPath from string at runtime now can.
- Errors carry Rimu spans (the `FromRimu` trait is already span-aware
  via `from_rimu_spanned`), so a wrong-type field can highlight the
  exact source location.
- `ParamValue` stops mirroring Rimu's `Value` for path variants — could
  shrink to a thin "validated" marker or be removed entirely.
- `SerdeValue` stops being a load-bearing crossing in lusid (still
  needed by Rimu internally for serde-shaped APIs, but not on the
  resource boundary).

**Cons**
- Without the derive macro, ~12 resources need hand-written impls.
  With the macro, the macro is non-trivial to write.
- Resource authors lose the wider serde ecosystem on this boundary
  (no `#[serde(default = "...")]`, `#[serde(flatten)]`,
  `#[serde(deserialize_with = "...")]`, etc.). Today only the tagged-
  enum and `Option<T>` features are in use, so this may be fine.
- Migration is per-resource: until all are converted, both code paths
  coexist.

**When to pick this:** when there's at least one concrete need to
discriminate HostPath-from-string at the resource boundary, OR when the
serde flattening is producing confusing error messages that a span-aware
`FromRimu` would fix. Pre-requisite: the `#[derive(FromRimu)]` macro, or
an explicit decision to hand-write impls.

---

## Option C — drop `ParamValue`; one-pass validate-and-extract

**What changes:** delete the `ParamValue` enum and its conversions.
`ParamType` directly produces typed Rust values. The schema is no longer
a structural matcher — it's a parser that *returns* the typed result.

This is the most radical option: it merges `validate` and
`from_rimu_spanned` into one pass, and removes the `ParamValue ↔ Value`
round-trip entirely.

### Sketch

```rust
// params/src/lib.rs
pub enum ParamType {
    String,
    Number,
    HostPath,
    TargetPath,
    List(Box<ParamType>),
    Object(Box<ParamType>),
    Struct(IndexMap<String, ParamField>),
    Union(Vec<IndexMap<String, ParamField>>),
}

impl ParamType {
    /// Validate `value` against `self` and produce a typed Rust value
    /// in one pass. Errors carry the offending span.
    pub fn parse<T: FromRimuTyped>(
        &self,
        value: Spanned<Value>,
    ) -> Result<T, Spanned<ParamParseError>>;
}

pub trait FromRimuTyped {
    fn from_rimu_typed(
        value: Spanned<Value>,
        ty: &ParamType,
    ) -> Result<Self, Spanned<ParamParseError>>
    where Self: Sized;
}
```

`ParamValues` (the map wrapper) stays — it's the keyed result of parsing
a struct schema. But its inner element type becomes the resource's typed
field, not a `ParamValue` enum.

Resources implement `FromRimuTyped` for their `Params` types, similar to
Option B but with the schema available during parsing — so the schema
guides the parse (e.g. union dispatch consults the schema's case list,
not a magic `state` field — or does both).

**Pros**
- One source of truth. The schema and the typed extraction are the
  same code path; no chance for them to drift.
- `ParamValue` and its 2× duplication of `Value` variants is gone.
- `validate` no longer "throws away" the work it just did — it produces
  the typed result.
- Cleaner conceptual model for new contributors.

**Cons**
- Big refactor. Touches every resource, plus the `validate` /
  `ParamValue` / `ParamValues` plumbing in `params/`, plus `core.rs`
  routing in `plan/`.
- Couples schema and Rust type more tightly — can't easily run a
  schema as a pure shape-checker without producing typed values.
  Today the schema can be inspected without running it (e.g. for
  documentation generation); under Option C that requires an "inspect
  only" trait variant.
- Designing the `FromRimuTyped` trait so it composes (lists of
  HostPaths, optional unions, etc.) takes care.
- Far more code to write before the first resource works again. A
  staged migration is harder than Option B (where resources can
  convert one at a time).

**When to pick this:** if Option B reveals that the `ParamValue` mirror
truly has no purpose, *and* there's an appetite for a multi-day
refactor. Probably not now — the cost/benefit is hard to justify until
Option B's pain points (if any) are concrete. Treat as "after B has
shipped and we know what we want."

---

## Recommendation

1. **Now**: Option A. Land the Rimu-side typed paths and update
   `ParamValue::from_rimu_spanned` / `into_rimu` to use them. Smallest
   change; preserves all existing behaviour.
2. **Soon**: write a `#[derive(FromRimu)]` proc-macro in `rimu-interop`,
   even before committing to Option B. It's useful for the existing
   `ParamType` / `ParamField` / `ParamTypes` impls too, and unblocks B
   if/when needed.
3. **Maybe**: Option B, resource-by-resource, when a real call site
   needs path-type discrimination at the resource boundary, OR when the
   serde error messages on `params` mismatches become a recurring user
   complaint.
4. **Probably not**: Option C, unless after Option B is fully in place
   we still find `ParamValue` is dead weight.

The order matters: each step is reversible and builds on the previous,
and steps 2–4 are individually justifiable on their own merits.
