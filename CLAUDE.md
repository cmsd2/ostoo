## Project rules

- When making significant changes, update the relevant docs in `docs/`.
- When writing or modifying PlusCal/TLA+ specs, follow [`specs/PLUSCAL.md`](specs/PLUSCAL.md).

## Coding style — concurrency

- **Never call `block_current_thread()` directly.** Use `WaitCondition::wait_while()` or the split `mark_blocked()` / `wait_until_unblocked()` pair. See [`docs/blocking-protocol.md`](docs/blocking-protocol.md).
- **One lock acquisition = one atomic step.** If two operations must be atomic (e.g. check + register waiter + mark blocked), they must be under the same lock. Never split check-then-act across separate lock/unlock cycles.
- **Mark blocked under the caller's lock.** Call `scheduler::mark_blocked()` before dropping the guard, so that `unblock()` is guaranteed to find `ThreadState::Blocked`.
- **Tag spec correspondence.** Annotate Rust code with `// [spec: file.tla Label]` comments to link to the PlusCal model. See [`specs/PLUSCAL.md`](specs/PLUSCAL.md) for the convention.
