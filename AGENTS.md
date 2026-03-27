# AI Agent Guide — lanyte-verify

Start every session with:

1. `/Users/davethompson/dev/lanytehq/AGENTS.md`
2. `/Users/davethompson/dev/lanytehq/lanyte-crucible/docs/guides/dev-warmup.md`
3. This repo's `REPOSITORY_SAFETY_PROTOCOLS.md`

## Working rules

- This repo is standalone. Do not add dependencies on any crate in `/Users/davethompson/dev/lanytehq/lanyte`.
- Follow CRT-013 at `/Users/davethompson/dev/lanytehq/lanyte-productbook-internal/content/projmgmt/core-runtime/CRT-013-lanyte-verify.md`.
- Keep the verifier surface synchronous. Runtime selection belongs to consumers.
- Keep Rust MSRV at `1.85.0`.
- Treat verifier result shape changes as downstream-facing API changes and pause for review at working checkpoints.
