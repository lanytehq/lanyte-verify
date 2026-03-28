# lanyte-verify

`lanyte-verify` is a standalone Rust library crate for verifying tool execution outcomes at
agent runtime.

It provides a common result model, a pluggable verifier registry, and built-in verification
strategies for file mutation, JSON output, and HTTP artifact reachability.

## Scope

- mutation verification via read-back and comparison
- output verification via constraints and lightweight schema checks
- provenance verification via structured citation and metadata evidence

The crate is intentionally standalone. It does not depend on the `lanyte` workspace and leaves
domain-specific verifiers to downstream consumers.

## Current status

CRT-013 implementation scaffold is in place.

- core verification types are serializable for downstream audit integration
- the `Verifier` trait exposes separate active and passive entry points
- `VerifierRegistry` dispatches by operation and returns `Skipped` when no verifier is registered
- built-in `FileVerifier` produces line diffs on content mismatches
- built-in `JsonVerifier` enforces built-in constraints and a minimal JSON Schema subset
- built-in `HttpVerifier` is feature-gated behind `http`

## Install

```sh
cargo test
```

Enable the HTTP verifier:

```sh
cargo test --features http
```
