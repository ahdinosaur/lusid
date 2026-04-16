# lusid-apply

Pipeline orchestrator. Loads a plan, builds the resource → state → change →
operation trees, schedules operations by dependency epoch, and executes them
— streaming progress as newline-delimited JSON
[`AppUpdate`](../apply-stdio)s on stdout for the [`lusid`](../lusid) TUI.

Shipped as both a library (`lusid_apply::apply`) and a binary (`lusid-apply`)
so the TUI can either spawn it as a subprocess or drive the library in-process.

## Pipeline

1. **Plan** — [`lusid_plan::plan`] evaluates Rimu, produces `PlanTree<ResourceParams>`.
2. **Resources** — each plan node expands into 1+ typed resources
   ([`map_plan_subitems`] scopes any intra-resource ids).
3. **ResourceStates** — async `Resource::state()` probes, one per leaf.
4. **ResourceChanges** — pure diff `(Resource, State) → Option<Change>`;
   `None` leaves are pruned.
5. **Operations** — each change expands into an operation subtree.
6. **Epoch scheduling** — [`lusid_causality::compute_epochs`] orders the
   operations into topological layers.
7. **Apply** — per-epoch, [`Operation::merge`] coalesces like-typed
   operations (e.g. multiple `apt install` → one multi-package call), then
   each is executed with its stdout/stderr streamed back as events.

Early-returns after phase 4 with "No changes to apply!" if the diff is empty.

## Protocol

Everything between `ResourceParams` and `OperationsApplyComplete` is emitted
to stdout. Tracing goes to stderr; nothing else should be written to stdout.

See [`apply-stdio`](../apply-stdio/README.md) for the full `AppUpdate` enum.

## CLI

```
lusid-apply --root <path> --plan <path.lusid> [--params '{"k":"v"}'] [--log info]
```
