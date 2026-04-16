# lusid-params

Parameter schemas and validation for lusid plans.

Every plan (and every core module) declares a `params` schema. This crate
defines the types involved, the Rimu → Rust conversion, and the validator.

## The three pieces

- **Schema (`ParamType`, `ParamField`, `ParamTypes`)** — what shape of value is
  accepted. `ParamTypes` is either a single `Struct` or a `Union` of struct
  cases.
- **Value (`ParamValue`, `ParamValues`)** — the parsed, typed value.
- **`validate()`** — type-checks a Rimu value object against a schema. For
  unions, first-match wins (cases tried in declaration order).

## Path-type conventions

- `HostPath` — a **relative** string, resolved at conversion time against the
  source `.lusid` file's directory. This is why Rimu spans must carry a real
  filesystem `SourceId`.
- `TargetPath` — an **absolute** string, used as-is on the managed machine.

## Spans

Schemas, values, and errors carry `Spanned<T>` so diagnostics can point at the
exact plan line. Preserve spans when adding new variants or errors — it's the
whole point of the rich error enums here.
