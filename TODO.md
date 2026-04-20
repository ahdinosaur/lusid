## Known limitations / follow-ups

### Tagged-secret concatenation is not redacted

`Redactor` is built once from the eager `Secrets` table, so it sees only
the original plaintexts, not any `prefix + secret` string a plan
constructs before handing off to a resource. A short prefix plus a
secret still contains the secret as a substring, so redaction usually
still works for long-enough secrets (`REDACT_MIN_LEN = 8`). For short
secrets or plans that transform the plaintext, the concatenated form is
not tracked. The tagged-value plumbing now makes it *possible* to walk
any `Value::Tagged { tag: "secret", .. }` reaching a resource and
register its inner string with the `Redactor` at apply time; not done
yet.

### Plaintext still lives as an ordinary `String` inside the evaluator

`Value::Tagged { inner: Box<Spanned<Value::String>>, .. }` wraps the
plaintext in the tag envelope but the inner is still a plain `String`.
Any copies Rimu makes during evaluation (function arguments, object
construction, arithmetic temporaries) live outside the `SecretBox` and
are not zeroised on drop. agenix / sops-nix sidestep this by passing
*filenames* through the evaluator and materialising secret contents at
activation time. Revisit if plaintext-in-evaluator becomes a concrete
threat (LSP plugin snapshotting values, debug dumps, etc.).

### `ValidateValueError::NullSecret` is effectively dead code

A typo in `ctx.secrets.<name>` returns `Null` from Rimu's object access
(both on `main` and on the `tagged` branch — `get_key` errors, it
doesn't return `Null`), so the `Value::Null` arm of `ParamType::Secret`
validation is never reached in practice. The real fix is a strict
`ctx.secrets` container that errors at access time (needs a Rimu hook,
e.g. a `Value::Secrets`-like container or per-object lookup callback).
Leave the arm in for now — it's dormant, not incorrect, and the right
fix is upstream.

### Merging `tagged` branch back into rimu's `main`

The rimu dep in `lusid/Cargo.toml` pins `branch = "tagged"`. Once the
tagged-value design is reviewed and merged to `main` (or tagged as a
release), switch this back to a normal git ref / crates.io version.
