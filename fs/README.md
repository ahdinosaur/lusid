# lusid-fs

Async filesystem helpers for lusid operations and resources.

Each function wraps a `tokio::fs` / `nix` / `filetime` call and maps the
underlying error into a rich [`FsError`] variant that always carries the
offending path(s). This means error messages surfaced by `lusid-apply` don't
need to re-construct context downstream.

Highlights:

- **`write_file_atomic` / `copy_file_atomic`** — write to a sibling temp file,
  copy metadata (from destination and source respectively), and rename. Readers
  never observe a partial write.
- **`change_owner` / `change_owner_by_id`** — Unix-only uid/gid changes,
  resolving user/group names via `nix`.
- **`copy_dir`** — currently shells out to `cp --recursive`. Portable-only
  across GNU coreutils Linuxes; see the note in the source.
