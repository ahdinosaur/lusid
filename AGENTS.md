# AGENTS.md

**lusid** is a Rust project for declarative machine configuration via “plans”, producing a dependency-aware resource/operation tree and applying it with a streaming TUI.

## What this project is

**Lusid** takes a `.lusid` plan (written in the **Rimu** language), optionally a parameters object, and:

1. Loads + evaluates the plan’s `setup(params, system)` function → returns a list of **PlanItem**s.
2. Converts PlanItems into either:
   - **Core modules** (`@core/*`) → become typed `ResourceParams` (apt/file/pacman today)
   - Or nested plans (module path) → recursively planned
3. Validates parameter schemas and values (with good span/source error reporting).
4. Builds a **causality tree** (nodes can have `id`, `before`, `after` dependencies).
5. Computes dependency **epochs** (topological layers).
6. Applies operations epoch-by-epoch, streaming structured UI updates as JSON to stdout.
7. The `lusid` CLI runs `lusid-apply-*` and renders a TUI from those updates.

Key design themes:
- Strong typing at the boundaries (params → typed structs).
- Span-aware diagnostics using Rimu `Spanned<T>` values.
- Tree-first architecture: nested `Tree` and arena-based `FlatTree`.
- Dependency ordering via `CausalityMeta { id, before, after }` and Kahn’s algorithm.

## Workspace map (high level)

You’ll mostly touch these crates:

### Planning & parameters

- **`plan/`**
  - `plan()` is the top-level planner.
  - `load.rs` parses + evaluates Rimu → `Plan`.
  - `eval.rs` calls the Rimu `setup` function.
  - `core.rs` maps `@core/*` modules to `lusid_resource` types.
  - `id.rs` defines `PlanId` and `PlanNodeId` (used for dependencies & display).
- **`params/`**
  - Parameter schema types (`ParamTypes`, `ParamType`, `ParamField`)
  - Parameter values (`ParamValues`, `ParamValue`)
  - Validation: `validate()` chooses a matching struct/union case and checks values.

### Resources & operations

- **`resource/`**
  - `ResourceType` trait: defines params schema, how params expand into resources, state fetching, diffing to changes, and mapping changes to operations.
  - Implementations: `apt`, `pacman`, `file`.
- **`operation/`**
  - Operation merge/apply logic by operation type.
  - Apply returns: `(Future completion, stdout stream, stderr stream)`.

### Apply pipeline + UI streaming
- **`lusid-apply/`**
  - Orchestrates the pipeline: plan → resources → state → changes → operations → apply.
  - Emits `AppUpdate` JSON lines to stdout.
- **`apply-stdio/`**
  - Defines `AppUpdate` and `AppView` state machine.
  - Contains a flat view tree model used by the TUI.

### CLI, system, infra
- **`lusid/`**
  - CLI (`clap`) + config loader (`lusid.toml`) + TUI renderer for `lusid-apply` JSON stream.
- **`system/`**
  - Detects local machine info: arch/os/user/hostname.
- **`vm/`**, **`ssh/`**
  - Dev/remote workflows: spawn QEMU VMs, sync files, run remote commands, etc.
- **`tree/`** and **`causality/`**
  - Generic tree and dependency epoch computation.
- **`store/`**
  - Currently only a local file store (`tokio::fs::read`).
  - `PlanId::Git` is currently **TODO** in `plan/src/id.rs`.

## The end-to-end data flow

If you need a “starting point” file for understanding the runtime behavior, read in this order:
1. `lusid-apply/src/lib.rs` (full pipeline)
2. `plan/src/lib.rs` (planning recursion + core modules)
3. `params/src/lib.rs` (schema/value validation)
4. `causality/src/epoch.rs` (dependency scheduling)
5. `lusid/src/tui.rs` (how updates are rendered)

### Planning (Rimu → PlanTree<ResourceParams>)
Entry point: `lusid_plan::plan(plan_id, params_value, store, system)`

Core steps:
- Read plan source via `Store::read(StoreItemId)` (currently LocalFile only).
- `load(code, plan_id)`:
  - `rimu::parse` → AST
  - `rimu::evaluate` → `Value`
  - `Plan::from_rimu_spanned(Value)` → `Spanned<Plan>`
- `validate(param_types, params_value)` decides which param struct applies (supports union).
- `evaluate(setup, params_value, params_struct, system)` calls the Rimu setup function.
- Each returned `PlanItem` becomes:
  - Core module resource params via `core_module(...)`, or
  - A nested plan (module treated as relative path) → recursion.

### Apply (PlanTree → Operations → execution)
Entry: `lusid_apply::apply(ApplyOptions { root_path, plan_id, params_json })`

Pipeline stages emitted as JSON updates:
1. `ResourceParams`
2. `ResourcesStart/ResourcesNode/ResourcesComplete`
3. `ResourceStatesStart/NodeStart/NodeComplete/Complete`
4. `ResourceChangesStart/Node/Complete(has_changes)`
5. `OperationsStart/Node/Complete`
6. `OperationsApplyStart` then per operation: stdout/stderr streaming updates and completion updates

Epoch ordering:
- `lusid_causality::compute_epochs(CausalityTree<Option<Node>, NodeId>)`

---

## “Gotchas” / invariants to preserve

### Spans and diagnostics are important
Many errors are `Spanned<...>` or embed `Span` to point to plan source locations. When adding new parsing/validation logic:
- Preserve spans where possible.
- Prefer returning `Spanned<Error>` variants when the error is attributable to a specific value.

### ParamType HostPath vs TargetPath
In `params`:
- `HostPath` expects a **relative** string; it is resolved relative to the source file directory using span source info.
- `TargetPath` expects an **absolute** path string.

If you add new path-like types, follow this pattern and be explicit about absolute/relative requirements.

### Causality IDs must be unique
`compute_epochs` fails on duplicate IDs across leaves/branches. Any new code generating ids should avoid collisions (or scope them like `map_plan_subitems()` does by minting a `scope_id`).

### FlatTree/arena semantics
Both `lusid_tree::FlatTree` and `lusid_apply_stdio::FlatViewTree`:
- Root index is always `0`.
- Missing nodes (`None`) are tolerated by lenient reconstruction.
- “Replace subtree” appends new nodes and prunes old descendants by setting them to `None`.

When you change flattening/replacement logic, keep these invariants.

### Streaming output protocol
`lusid-apply` emits **newline-delimited JSON** `AppUpdate` messages to stdout.
The `lusid` TUI expects this exact protocol. Avoid printing human text to stdout from `lusid-apply`; use tracing/logging to stderr.


## Build / run / test (agent checklist)

### Typical commands
- Build workspace:
  - `cargo build`
- Run CLI:
  - `cargo run -p lusid -- --help`
- Run apply binary (manual):
  - `cargo run -p lusid-apply -- --root <root> --plan <path/to/plan.lusid> --log info --params '{"k":"v"}'`
- Run tests:
  - `cargo test`

If you add new behavior, add tests where they naturally fit:
- Parameter validation: `params` crate unit tests.
- OS/system parsing: `system` crate tests (already has some).
- Epoch computation: `causality` crate tests are appropriate.

## Coding style expectations (match existing code)

- Error handling uses `thiserror` + rich enums; avoid `anyhow`-style catchalls.
- Many crates use `displaydoc::Display` for error messages; follow that pattern.
- Prefer small pure functions
- Keep public APIs conservative: prefer adding new types/functions instead of changing signatures.
- Maintain `Clone` friendliness when types are used in trees/flat arenas.

---

## Where to make common changes

### Add a new core module resource (e.g. `@core/service`)
1. Implement a new `ResourceType` in `resource/src/resources/<new>.rs`
2. Add it to:
   - `resource/src/resources/mod.rs`
   - `resource/src/lib.rs` enums: `ResourceParams`, `Resource`, `ResourceState`, `ResourceChange`, plus matching logic
3. Add core mapping in `plan/src/core.rs`:
   - `match core_module_id { New::ID => ... }`
4. Add operations in `operation/` if needed.

### Add new parameter types or validation rules
- `params/src/lib.rs` is the single source of truth.
- Update:
  - `ParamType` / `ParamValue` conversions
  - `validate_type` rules
  - `FromRimu` parsing for schemas
- Add tests for:
  - Correct match
  - Mismatch errors include useful spans

### Add store backends (git/http/etc.)
- Extend `StoreItemId` and `Store` in `store/`.
- Implement a `SubStore` with cache directory support.
- Update `PlanId -> StoreItemId` conversion in `plan/src/id.rs`.

## Safety and operational concerns

This project runs privileged operations (`sudo apt-get`, `sudo pacman`, filesystem ownership changes). When adding new operations:
- Ensure commands are non-interactive.
- Avoid leaking secrets in logs/structured UI updates.
- Keep stdout/stderr streaming for long-running commands.

## Quick glossary

- **Rimu**: embedded language used for `.lusid` plans.
- **Spanned**: value annotated with source span for diagnostics.
- **Plan**: parsed/evaluated Rimu object containing `setup`.
- **PlanItem**: an entry returned by setup, either core module or nested plan.
- **ResourceParams**: typed configuration definition (user-facing).
- **Resource**: atomized resource node(s) derived from params.
- **State**: observed current system state for a resource.
- **Change**: computed delta from state to desired.
- **Operation**: executable action(s) derived from change.
- **Epoch**: dependency layer computed from causality constraints.

## Before submitting changes (AI agent self-check)

- Does the change preserve span-aware errors where applicable?
- Does it maintain the stdout JSON protocol from `lusid-apply`?
- Did you avoid printing non-JSON to stdout in apply?
- Are causality IDs still unique and dependencies valid?
- Are new operations safe/non-interactive and appropriately `sudo()`-wrapped?
- Did you add/adjust tests for logic-heavy changes?

## 5) What is unfinished / likely tasks

Known TODOs visible from the code you were given:
- `PlanId::Git` conversion to `StoreItemId` is `todo!()` (git-backed store not implemented).
- `lusid` CLI remote workflows:
  - `cmd_remote_apply`, `cmd_remote_ssh` are `todo!()`.
- There is some duplication between `lusid_http` and `vm/src/http.rs` (two HTTP clients with similar code).
- Expect ongoing iteration in parameter schema/validation and plan evaluation.

When implementing these:
- Prefer extending `store/` with a new `SubStore` rather than embedding fetch logic in planning.
- Keep the plan loading interface stable: `Store::read(StoreItemId)` → `Vec<u8>`.
