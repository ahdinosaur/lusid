# lusid-causality

Dependency ordering for tree-structured workloads.

Wraps [`lusid_tree::Tree`] with [`CausalityMeta`]: each node gets an optional `id`
plus `requires` and `required_by` lists of ids. [`compute_epochs`] flattens the
tree into topologically-sorted layers ("epochs") using Kahn's algorithm — each
epoch holds nodes with no remaining dependencies, safe to run in parallel.

## Semantics

- **Branch-inherited constraints.** A branch's `requires` / `required_by` apply
  to every descendant leaf.
- **Group ids.** A branch's `id` refers to the set of all descendant leaves, so
  depending on a branch id means depending on every leaf under it.
- **Marker leaves.** Leaves carrying `None` as their node are kept in the
  dependency graph (so their ids still resolve) but excluded from the epoch
  output.
- **Unique ids.** All ids must be unique across the tree; duplicates are a hard
  error.

## Used by

`lusid-apply` calls `compute_epochs` on the resource/operation tree, then runs
each epoch concurrently before moving to the next. See the top-level `AGENTS.md`
for the full pipeline.
