# lusid-operation

Concrete mutations that actually run on the target machine — `apt install`,
`write file`, `git clone`, etc. Operations are the leaves of the per-epoch
causality tree applied by `lusid-apply`.

## `OperationType` trait

Every operation family (apt, pacman, file, command, git) implements
[`OperationType`], which supplies two things:

- **`merge`** — coalesce same-family operations scheduled in the same epoch.
  Package managers union their install sets; order-sensitive families (file,
  command, git) keep operations as-is.
- **`apply`** — start the operation and return `(completion_future,
  stdout_stream, stderr_stream)`. The caller drives all three concurrently so
  output streams live to the TUI.

## Dispatcher enums

`Operation`, `OperationApplyError`, `OperationApplyOutput`,
`OperationApplyStdout`, and `OperationApplyStderr` wrap the per-family types.
The three `ApplyXxx` enums use `pin_project` so `Future::poll` / `AsyncRead::poll_read`
forward to the active variant without extra boxing.

## Privileged operations

`apt` and `pacman` wrap commands with `Command::sudo()`; `git` and `command`
do not. Follow the same pattern when adding new families: only escalate when
the underlying tool actually needs root.

## Streaming output

Commands that spawn child processes (apt, pacman, command, git) expose the
child's `ChildStdout` / `ChildStderr` directly. The `file` family has no child
process, so it returns `tokio::io::empty()` streams.
