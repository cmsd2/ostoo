# getrandom (nr 318)

## Linux Signature

```c
ssize_t getrandom(void *buf, size_t buflen, unsigned int flags);
```

## Description

Fills a buffer with random bytes. Used by Rust's `HashMap` for hash seed randomisation and by musl for stack canary initialisation.

## Current Implementation

Uses a simple xorshift64\* PRNG seeded from the x86 TSC (Time Stamp Counter via `rdtsc`). Fills the user buffer byte-by-byte from the PRNG state. The `flags` parameter is accepted but ignored.

**Note:** This is not cryptographically secure. It provides enough entropy for `HashMap` seeds and similar non-security use cases.

**Source:** `osl/src/dispatch.rs` — `sys_getrandom`

## Errors

| Errno | Condition |
|-------|-----------|
| `-EFAULT` (-14) | Invalid buffer pointer |

## Future Work

- Seed from a proper entropy source (e.g. `RDSEED`/`RDRAND` instructions).
- Distinguish `GRND_RANDOM` vs `GRND_NONBLOCK` flags.
