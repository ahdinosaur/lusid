# lusid-tree

Generic tree data structures used throughout lusid.

Provides two complementary representations:

- **`Tree<Node, Meta>`** — a recursive, nested tree. `Branch` variants own their
  children directly; `Leaf` variants hold a value. Every node carries a `Meta`
  payload alongside the variant-specific data.

- **`FlatTree<Node, Meta>`** — an arena-backed flat tree. Nodes live in a
  `Vec<Option<FlatTreeNode>>` and reference each other by index. The `Option`
  layer lets us tombstone removed nodes without shifting indices, which is
  important because plan transformations hold onto indices as stable identifiers.

`FlatTree` is the workhorse: the apply pipeline turns a `Tree` into a `FlatTree`
and then runs a series of `map_*` passes on it — each pass transforming leaves
(e.g. `PlanItem → ResourceParams → Resource → Operation`). The async map
variants take `write_start` / `write_update` callbacks, which is how
`lusid-apply` emits per-node progress as newline-delimited JSON for the TUI.

## Invariants

- Root is always at index 0.
- Lenient reconstruction: missing or out-of-bounds children are tolerated.
- `replace_tree(index)` recursively tombstones existing descendants, then appends
  new children to the end of the arena; the original node keeps its slot.
- Depth-first traversal is post-order.
