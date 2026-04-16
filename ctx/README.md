# lusid-ctx

Shared runtime context for a lusid invocation.

A `Context` bundles:

- **`root`**: the plan root directory — resolved against when params declare
  `HostPath` (relative) inputs.
- **`Paths`**: platform-aware data / cache / runtime directories (XDG on Linux,
  `~/Library` on macOS, `%LOCALAPPDATA%`/`%TEMP%` on Windows).
- **`HttpClient`**: a reusable HTTP client for resources that fetch remote
  content.

Construct once at the top of a run and hand it through to planning and apply.
When new cross-cutting dependencies appear (secrets, caches, credentials), add
them here rather than threading them as separate arguments.
