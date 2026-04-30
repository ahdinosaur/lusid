# lusid dotfiles example

A minimal dotfiles-style setup demonstrating `state: "sourced"` for
`@core/file` and `@core/directory`.

## What it does

Run `lusid local apply` and the plan creates two symlinks under `$HOME`:

- `~/.zshrc` → `examples/dotfiles/files/zshrc`
- `~/.config/helix/` → `examples/dotfiles/files/helix/`

Edits to the source files in this repo show up immediately at the symlink
targets — no re-apply needed.

## How `sourced` behaves across apply modes

Lusid materialises `state: "sourced"` differently depending on where the apply
binary is running:

| Apply mode | `@core/file state: "sourced"` | `@core/directory state: "sourced"` |
| --- | --- | --- |
| `local apply` | symlink at `path` → host `source` | symlink at `path` → host `source` |
| `dev apply` / `remote apply` | atomic byte copy of `source` to `path` | recursive `cp -r` of `source` to `path` |

Local apply assumes the operator is editing files they want to keep editing,
and symlinks let those edits propagate. Dev/remote apply assumes the operator's
machine isn't reachable from the target, so the bytes have to live on the
target — `lusid` SFTPs the plan + sources during the dev/remote flow before
running apply.

## Running

```sh
lusid --config examples/dotfiles/lusid.toml local apply
```

Override `home` in `lusid.toml` (or pass `--params '{"home": "/home/<you>"}'`)
so the symlinks land somewhere you actually own.
